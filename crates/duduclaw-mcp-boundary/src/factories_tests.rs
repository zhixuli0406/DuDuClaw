use super::*;
use crate::RequestContext;
use std::sync::atomic::{AtomicUsize, Ordering};
use duduclaw_isolation::{capability::{CapabilityId, CapabilityRecord, EmployeeBinding, Lane,
    Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use crate::{execution::AuthorizedExecutionPlan, tool_policy::AuthorizedToolCall, parsing::{TableKind, table_request}, tool_policy::ToolPolicy};
use serde_json::json;

struct TestLive;
impl duduclaw_isolation::capability::LiveRequestVerifier for TestLive {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
fn context(tool: &str) -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new(tool.into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
            ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() },
            ResourceGrant::Workspace { handle: Text::new("other".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
        nonce: [5; 32], jti: [6; 16],
    }, Arc::new(TestLive))
}
fn table_call() -> AuthorizedToolCall {
    crate::tool_policy::authorize(&context("csv_read"), "csv_read",
        &json!({"workspace":"work","path":"table.csv"}), resolver(false, false).as_ref()).unwrap()
}
fn table_input(call: &AuthorizedToolCall, file: std::fs::File) -> AuthorizedFile {
    AuthorizedFile::for_test(call, file)
}

struct Resolver { calls: AtomicUsize, changes: bool, refuses: bool }
impl ResourceResolver for Resolver {
    fn resolve(&self, context: &RequestContext, _: &ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        if self.refuses { return Err(VerificationError::Unavailable); }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Workspace { handle: Text::new(if self.changes && call > 0 { "other" } else { "work" }.into()).unwrap() }])
    }
}
fn resolver(changes: bool, refuses: bool) -> Arc<Resolver> {
    Arc::new(Resolver { calls: AtomicUsize::new(0), changes, refuses })
}
struct Plans(AtomicUsize);
impl ExecutionPlanFactory for Plans {
    fn supports(&self, name: &str) -> bool { name == "execute_program" }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        Box::pin(async move { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) })
    }
    fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall, _: duduclaw_isolation::execution::RunHandle)
        -> PolicyFuture<'a, AuthorizedExecutionPlan> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
}

#[test]
fn isolated_boot_missing_factories_stay_unavailable() {
    let factories = GatewayFactories::assemble(None, None, resolver(false, false), None).unwrap();
    assert!(factories.execution.is_none());
    assert!(factories.parser.is_none());
    assert!(GatewayFactories::assemble(None, Some(Arc::new(Plans(AtomicUsize::new(0)))),
        resolver(false, false), None).is_err());
}


fn options() -> TableOptions {
    TableOptions { kind: TableKind::Csv, delimiter: b',', has_header: true, offset: 0, limit: 2, sheet: None }
}
fn table() -> Value { json!({"columns":["name"],"rows":[{"name":"員工"}],"truncated":false}) }

#[tokio::test]
async fn isolated_worker_adapter_delivers_authorized_fd_and_bounded_options() {
    let invoked = Arc::new(AtomicUsize::new(0));
    let counter = invoked.clone();
    let parser = GatewayTableParser::new(Arc::new(move |request, file, options| {
        counter.fetch_add(1, Ordering::SeqCst);
        assert!(file.metadata().unwrap().is_file());
        assert_eq!(request.inputs.as_slice().len(), 1);
        assert_eq!(options.limit, 2);
        Ok(table())
    }));
    let file = tempfile::tempfile().unwrap();
    let request = table_request(&file, &options()).unwrap();
    let call = table_call();
    let input = table_input(&call, file);
    let response = parser.parse(call, request.clone(), input, options()).await.unwrap();
    assert_eq!(crate::parsing::decode_table_response(&request, &options(), response).unwrap()["row_count"], 1);
    assert_eq!(invoked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_worker_adapter_rejects_mismatched_input_and_untrusted_result() {
    let invoked = Arc::new(AtomicUsize::new(0));
    let counter = invoked.clone();
    let parser = GatewayTableParser::new(Arc::new(move |_, _, _| {
        counter.fetch_add(1, Ordering::SeqCst);
        let mut value = table(); value["path"] = json!("forged"); Ok(value)
    }));
    let file = tempfile::tempfile().unwrap();
    let mut request = table_request(&file, &options()).unwrap();
    request.inputs = List::new(vec![duduclaw_isolation::worker::InputFd { slot: 0,
        format: duduclaw_isolation::worker::ContentFormat::Text, size_bytes: 100 }]).unwrap();
    let call = table_call();
    let input = table_input(&call, file);
    assert!(parser.parse(call, request, input, options()).await.is_err());
    assert_eq!(invoked.load(Ordering::SeqCst), 0);
    let file = tempfile::tempfile().unwrap();
    let request = table_request(&file, &options()).unwrap();
    let call = table_call();
    let input = table_input(&call, file);
    assert!(parser.parse(call, request, input, options()).await.is_err());
    assert_eq!(invoked.load(Ordering::SeqCst), 1);
}

#[test]
fn isolated_worker_adapter_bounds_utf8_result_chunks() {
    let file = tempfile::tempfile().unwrap();
    let request = table_request(&file, &options()).unwrap();
    let value = json!({"columns":["name"],"rows":[{"name":"員".repeat(2700)}],"truncated":false});
    let response = encode_table_result(&request, value).unwrap();
    assert!(crate::parsing::decode_table_response(&request, &options(), response).is_ok());
    assert!(encode_table_result(&request, json!("員".repeat(MAX_RESULT_TEXT_BYTES))).is_err());
}

#[tokio::test]
async fn table_parser_refuses_a_token_from_another_call_before_transport() {
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let parser = GatewayTableParser::new(Arc::new(move |_, _, _| {
        invoked.fetch_add(1, Ordering::SeqCst); Ok(table())
    }));
    let original = table_call();
    let other = crate::tool_policy::authorize(&context("csv_read"), "csv_read",
        &json!({"workspace":"work","path":"other.csv"}), resolver(false, false).as_ref()).unwrap();
    let file = tempfile::tempfile().unwrap();
    let request = table_request(&file, &options()).unwrap();
    assert_eq!(parser.parse(other, request, table_input(&original, file), options()).await,
        Err(VerificationError::BindingMismatch));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct Revocable(std::sync::atomic::AtomicBool);
impl duduclaw_isolation::capability::LiveRequestVerifier for Revocable {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
        if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
    }
}
fn revocable_call(live: Arc<Revocable>) -> AuthorizedToolCall {
    let context = RequestContext::with_live_verifier(context("csv_read").record().clone(), live);
    crate::tool_policy::authorize(&context, "csv_read", &json!({"workspace":"work","path":"table.csv"}),
        resolver(false, false).as_ref()).unwrap()
}

#[test]
fn table_queued_on_blocking_pool_rechecks_revocation_before_transport() {
    use std::{future::Future, task::Poll};
    let runtime = tokio::runtime::Builder::new_current_thread().max_blocking_threads(1).enable_all().build().unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let (entered, ready) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || { entered.send(()).unwrap(); wait.recv().unwrap(); });
    ready.recv().unwrap();
    let live = Arc::new(Revocable(std::sync::atomic::AtomicBool::new(true)));
    let invocations = Arc::new(AtomicUsize::new(0));
    let count = invocations.clone();
    runtime.block_on(async {
        let parser = GatewayTableParser::new(Arc::new(move |_, _, _| {
            count.fetch_add(1, Ordering::SeqCst); Ok(table())
        }));
        let call = revocable_call(live.clone());
        let file = tempfile::tempfile().unwrap();
        let request = table_request(&file, &options()).unwrap();
        let input = table_input(&call, file);
        let future = parser.parse(call, request, input, options());
        tokio::pin!(future);
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(())
        }).await;
        live.0.store(false, Ordering::SeqCst);
        release.send(()).unwrap();
        assert_eq!(future.await, Err(VerificationError::Revoked));
        blocker.await.unwrap();
    });
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
}

#[test]
fn authorized_input_rechecks_authority_at_each_snapshot_read() {
    use std::io::{Read, Seek, Write};
    let live = Arc::new(Revocable(std::sync::atomic::AtomicBool::new(true)));
    let call = revocable_call(live.clone());
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"authorized").unwrap(); file.rewind().unwrap();
    let mut input = table_input(&call, file);
    let mut buffer = [0; 1];
    assert_eq!(input.read(&mut buffer).unwrap(), 1);
    live.0.store(false, Ordering::SeqCst);
    assert_eq!(input.read(&mut buffer).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn dropped_table_request_fences_queued_job_without_revoking_connection() {
    use std::task::Poll;
    let runtime = tokio::runtime::Builder::new_current_thread().max_blocking_threads(1).enable_all().build().unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let (entered, ready) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || { entered.send(()).unwrap(); wait.recv().unwrap(); });
    ready.recv().unwrap();
    let invocations = Arc::new(AtomicUsize::new(0));
    let count = invocations.clone();
    runtime.block_on(async {
        let parser = GatewayTableParser::new(Arc::new(move |_, _, _| {
            count.fetch_add(1, Ordering::SeqCst); Ok(table())
        }));
        let call = table_call();
        let file = tempfile::tempfile().unwrap();
        let request = table_request(&file, &options()).unwrap();
        let input = table_input(&call, file);
        let mut future = parser.parse(call, request, input, options());
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending()); Poll::Ready(())
        }).await;
        drop(future);
        release.send(()).unwrap();
        blocker.await.unwrap();
        // One blocking thread guarantees this barrier runs after the queued job.
        tokio::task::spawn_blocking(|| ()).await.unwrap();
    });
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
}
