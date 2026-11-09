use super::*;
use duduclaw_parse_worker::{bounded::{List, Text}, client::WorkerSession,
    worker::{ParseError, TableOutput}};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn isolated_worker_input_growth_is_error_not_truncation() {
    use std::io::{Seek, Write};
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"abcd").unwrap();
    file.rewind().unwrap();
    let mut limited = LimitedInput { file, remaining: 3 };
    let mut result = Vec::new();
    assert!(limited.read_to_end(&mut result).is_err());
    assert_eq!(result, b"abc");
}

fn options() -> TableOptions {
    TableOptions { kind: TableKind::Csv, delimiter: b',', has_header: true, offset: 0, limit: 2, sheet: None }
}
fn response(request: &ParseRequest) -> ParseResponse {
    ParseResponse { version: request.version, request_id: request.request_id,
        result: ParseResult::Complete { items: List::new(vec![ParsedItem::Table {
            input_slot: 0, output: TableOutput {
                columns: List::new(vec![Text::new("name".into()).unwrap()]).unwrap(),
                rows: List::new(vec![List::new(vec![TableCell::String(Text::new("員工".into()).unwrap())]).unwrap()]).unwrap(),
                truncated: false, sheet: None, sheets: List::new(vec![]).unwrap(),
            },
        }]).unwrap() } }
}

#[cfg(target_os = "linux")]
#[test]
fn isolated_worker_launcher_session_roundtrip() {
    struct Launcher(Arc<AtomicUsize>);
    struct Session(Arc<AtomicUsize>);
    impl WorkerLauncher for Launcher {
        fn supports_live_authority(&self) -> bool { true }
        fn launch(&self, request: &ParseRequest, bytes: u64) -> Result<Box<dyn WorkerSession>, ParseError> {
            assert_eq!(request.operation, ParseOperation::ReadTable);
            assert!(bytes >= request.inputs.as_slice()[0].size_bytes);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Session(self.0.clone())))
        }
    }
    impl WorkerSession for Session {
        fn exchange_checked(&mut self, request: &ParseRequest, files: &[File],
            verify: &dyn Fn() -> Result<(), ParseError>) -> Result<ParseResponse, ParseError> {
            verify()?;
            self.exchange(request, files)
        }
        fn exchange(&mut self, request: &ParseRequest, files: &[File]) -> Result<ParseResponse, ParseError> {
            assert_eq!(files.len(), 1);
            assert!(files[0].metadata().unwrap().is_file());
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(response(request))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let _services = install_worker_services(Box::new(Launcher(calls.clone()))).unwrap();
    let file = authorized_input();
    let request = client::request(ParseOperation::ExtractDocument, worker::ContentFormat::Text, 0);
    assert_eq!(invoke_table(request, file, options()).unwrap()["rows"][0]["name"], "員工");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let mut request = client::request(ParseOperation::ReadTable, worker::ContentFormat::Text, 0);
    request.options = Some(ParseOptions::Table(worker::TableOptions { format: worker::TableFormat::Csv,
        delimiter: b',', has_header: true, offset: 0, limit: 2, sheet: None }));
    let reply = invoke_authorized_input(request.clone(), authorized_input()).unwrap();
    assert!(reply.response.validate_for(&request).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(install_worker(Box::new(Launcher(calls))).is_err());
}

#[cfg(target_os = "linux")]
fn authorized_input() -> AuthorizedFile {
    use duduclaw_isolation::{capability::{CapabilityRecord, CapabilityId, EmployeeBinding, Lane,
        LiveRequestVerifier, Principal, RequestContext, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
    struct Live;
    impl LiveRequestVerifier for Live {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
    }
    let employee = EmployeeId([1; 16]);
    let context = RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([2; 32])), employee, run: RunId([3; 16]), task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new("csv_read".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [5; 32], jti: [6; 16],
    }, Arc::new(Live));
    let directory = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::write(directory.path().join("table.csv"), b"").unwrap();
    let mut roots = duduclaw_mcp_boundary::resources::ResourceRoots::default();
    roots.insert("work".into(), employee, File::open(directory.path()).unwrap()).unwrap();
    roots.authorize_file(&context, "csv_read", &json!({"workspace":"work","path":"table.csv"})).unwrap().1
}

#[test]
fn isolated_worker_result_rechecks_correlation_and_row_width() {
    let mut request = client::request(ParseOperation::ReadTable, worker::ContentFormat::Text, 0);
    request.options = Some(ParseOptions::Table(worker::TableOptions { format: worker::TableFormat::Csv,
        delimiter: b',', has_header: true, offset: 0, limit: 2, sheet: None }));
    assert!(table_value(&request, &options(), response(&request)).is_ok());
    let mut forged = response(&request);
    forged.request_id = duduclaw_isolation::ids::RequestId([0; 16]);
    assert!(table_value(&request, &options(), forged).is_err());
    let mut malformed = response(&request);
    if let ParseResult::Complete { items } = &mut malformed.result {
        let mut values = items.as_slice().to_vec();
        if let ParsedItem::Table { output, .. } = &mut values[0] {
            output.rows = List::new(vec![List::new(vec![]).unwrap()]).unwrap();
        }
        *items = List::new(values).unwrap();
    }
    assert!(table_value(&request, &options(), malformed).is_err());
}

#[test]
fn isolated_worker_startup_refuses_launcher_without_live_authority_transport() {
    struct Unchecked;
    impl WorkerLauncher for Unchecked {
        fn launch(&self, _: &ParseRequest, _: u64) -> Result<Box<dyn WorkerSession>, ParseError> {
            panic!("unchecked launcher must fail during installation")
        }
    }
    assert!(install_worker_services(Box::new(Unchecked)).is_err());
}
