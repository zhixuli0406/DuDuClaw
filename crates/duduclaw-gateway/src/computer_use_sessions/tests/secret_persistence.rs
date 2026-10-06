//! BT5: actual signed HTTP and operator RPC responses, live SQLite and log bytes.
use super::*;
use std::io::Write;
use std::path::Path;
use tracing::instrument::WithSubscriber;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct CapturedConfirmers {
    inner: FakeConfirmers,
    messages: Capture,
}
struct CapturedSender {
    inner: Box<dyn ChannelSender>,
    messages: Capture,
}
#[async_trait]
impl ConfirmerResolver for CapturedConfirmers {
    async fn context(&self, agent: &str, turn: &str) -> Option<crate::approval::DecisionContext> {
        self.inner.context(agent, turn).await
    }
    async fn resolve(&self, agent: &str, turn: &str) -> Option<Box<dyn ChannelSender>> {
        Some(Box::new(CapturedSender {
            inner: self.inner.resolve(agent, turn).await?,
            messages: self.messages.clone(),
        }))
    }
}
#[async_trait]
impl ChannelSender for CapturedSender {
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        self.messages
            .0
            .lock()
            .unwrap()
            .extend_from_slice(text.as_bytes());
        self.inner.send_text(text).await
    }
    async fn send_photo(&self, png: &[u8], caption: &str) -> Result<(), ChannelSendError> {
        self.messages
            .0
            .lock()
            .unwrap()
            .extend_from_slice(caption.as_bytes());
        self.inner.send_photo(png, caption).await
    }
    async fn request_confirmation(
        &self,
        prompt: &str,
        screenshot: Option<&[u8]>,
        timeout: u64,
    ) -> Result<bool, ChannelSendError> {
        self.messages
            .0
            .lock()
            .unwrap()
            .extend_from_slice(prompt.as_bytes());
        self.inner
            .request_confirmation(prompt, screenshot, timeout)
            .await
    }
    fn channel_type(&self) -> &'static str {
        self.inner.channel_type()
    }
}

fn clean(bytes: &[u8], marker: &str, medium: &str) {
    assert!(
        !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
        "BT5 plaintext found in {medium}"
    );
}

fn scan_files(dir: &Path, marker: &str) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            scan_files(&path, marker);
        } else {
            clean(
                &std::fs::read(&path).unwrap(),
                marker,
                &path.display().to_string(),
            );
        }
    }
}

fn scan_database(conn: &rusqlite::Connection, home: &Path, marker: &str) {
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM \"{quoted}\""))
            .unwrap();
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for col in 0..columns {
                use rusqlite::types::ValueRef;
                if let ValueRef::Text(bytes) | ValueRef::Blob(bytes) = row.get_ref(col).unwrap() {
                    clean(bytes, marker, &format!("SQLite {table} column {col}"));
                }
            }
        }
    }
    for name in ["approvals.db", "approvals.db-wal", "approvals.db-shm"] {
        let path = home.join(name);
        assert!(path.is_file(), "live SQLite medium must exist: {name}");
        clean(&std::fs::read(path).unwrap(), marker, name);
    }
    scan_files(home, marker);
}

async fn rpc_scan(
    handler: &crate::handlers::MethodHandler,
    conn: &rusqlite::Connection,
    marker: &str,
) {
    let admin = duduclaw_auth::UserContext::admin_fallback();
    let mut requests = vec![
        ("approvals.list", json!({})),
        ("approvals.operations", json!({})),
    ];
    for (table, column, method, param) in [
        ("approvals", "id", "approvals.list", "id"),
        (
            "approval_operations",
            "operation_id",
            "approvals.operations",
            "operation_id",
        ),
    ] {
        let ids: Vec<String> = conn
            .prepare(&format!("SELECT {column} FROM {table}"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for id in ids {
            requests.push((method, json!({(param): id})));
        }
    }
    for (method, params) in requests {
        let frame = handler.handle(method, params, &admin).await;
        assert!(
            matches!(frame, crate::protocol::WsFrame::Response { ok: true, .. }),
            "BT5 RPC failed: {method}"
        );
        clean(&serde_json::to_vec(&frame).unwrap(), marker, method);
    }
}

#[tokio::test]
async fn bt5_type_deny_approve_signed_routes_never_persist_plaintext_in_live_media_or_rpc() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    async {
        let tmp = home();
        let h = tmp.path();
        let marker = format!("BT5_PRIVATE_TYPE_{}", uuid::Uuid::new_v4().as_simple());
        let state = Arc::new(FakeState::default());
        *state.title.lock().unwrap() = Some(Ok("Bitwarden".into()));
        let messages = Capture::default();
        let mgr = Arc::new(manager(h, &state, IDLE_TIMEOUT).with_confirmers(Arc::new(
            CapturedConfirmers {
                inner: FakeConfirmers(Arc::new(AtomicU32::new(0)), h.to_path_buf()),
                messages: messages.clone(),
            },
        )));
        insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
        // Keep a normal SQLite connection alive before writes; WAL/SHM must be
        // observed while live, rather than silently skipped after final close.
        let broker = crate::approval::ApprovalBroker::open(h).unwrap();
        let conn = rusqlite::Connection::open(h.join("approvals.db")).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;
            PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        let handler = crate::handlers::MethodHandler::new(h.to_path_buf()).await;
        let router = http::router(mgr.clone());
        for (turn, expected) in [("no", false), ("yes", true)] {
            let body = serde_json::to_vec(&json!({"op":"action", "turn_id":turn,
                "action":{"type":"type", "text":marker}}))
            .unwrap();
            let (status, response) =
                call(router.clone(), loopback(), headers(h, "alice", &body), body).await;
            clean(
                &serde_json::to_vec(&response).unwrap(),
                &marker,
                "signed computer-use action response",
            );
            assert_eq!(response["ok"], expected);
            assert_eq!(status == 200, expected);
            if expected {
                let executed = state.executed.lock().unwrap();
                assert_eq!(executed.len(), 1);
                assert!(
                    matches!(&executed[0], ComputerAction::Type { text } if text == &marker),
                    "backend must receive the original Type, not a digest or substitute"
                );
            } else {
                assert!(state.executed.lock().unwrap().is_empty());
            }
            let outbound = messages.0.lock().unwrap();
            assert!(
                !outbound.is_empty(),
                "real confirmation delivery must be captured"
            );
            clean(
                &outbound,
                &marker,
                "confirmation send_text/request prompt capture",
            );
            drop(outbound);
            scan_database(&conn, h, &marker);
            rpc_scan(&handler, &conn, &marker).await;
        }
        // The same plaintext request also exercises the genuine denial audit
        // writer via denied_tools; computer_use=false ends the session but does
        // not emit tool_calls.jsonl. Confirmation refusal and successful
        // execution have browser rows.
        write_agent(
            h,
            "alice",
            "[capabilities]\ncomputer_use=true\ndenied_tools=['computer_type']\n",
        );
        let body =
            serde_json::to_vec(&json!({"op":"action", "action":{"type":"type", "text":marker}}))
                .unwrap();
        let (status, response) = call(router, loopback(), headers(h, "alice", &body), body).await;
        assert_eq!(status, ErrorCode::Forbidden.http_status());
        assert_eq!(response["code"], "forbidden");
        assert_eq!(state.executed.lock().unwrap().len(), 1);
        clean(
            &serde_json::to_vec(&response).unwrap(),
            &marker,
            "denied_tools refusal response",
        );
        for name in ["audit/browser/audit.jsonl", "tool_calls.jsonl"] {
            let path = h.join(name);
            let bytes = std::fs::read(&path).unwrap_or_else(|error| {
                panic!("real audit writer must produce {}: {error}", path.display())
            });
            assert!(!bytes.is_empty(), "empty audit medium: {name}");
            clean(&bytes, &marker, name);
        }
        // security_audit.jsonl is not emitted by these CU/broker paths; the
        // recursive scan covers it if a future real writer starts emitting it.
        scan_database(&conn, h, &marker);
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        scan_database(&conn, h, &marker);
        drop(broker);
        drop(conn);
        let reopened = rusqlite::Connection::open(h.join("approvals.db")).unwrap();
        // Reading a WAL database establishes its real sidecars again.
        reopened
            .execute_batch("PRAGMA journal_mode=WAL;
                SELECT count(*) FROM approvals;")
            .unwrap();
        scan_database(&reopened, h, &marker);
        let logs = capture.0.lock().unwrap();
        assert!(
            !logs.is_empty(),
            "tracing capture must contain actual gateway events"
        );
        clean(
            &logs,
            &marker,
            "gateway tracing capture (test sink, not production log file)",
        );
    }
    .with_subscriber(subscriber)
    .await;
}
