use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal, ResourceGrant}, ids::{EmployeeId, RunId, Sha256Digest}};
use crate::{parsing::{TableKind, TableOptions, table_request}, tool_policy::ToolPolicy};

// The generic bridge fixture is a Worker route that still selects a workspace
// (`model_load`); H12a own-skill tools pin an exact own-employee selection and
// have their own tests in `tool_policy_tests.rs`.

struct TestLive;
impl duduclaw_isolation::capability::LiveRequestVerifier for TestLive {
    fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
}
fn context() -> RequestContext {
    RequestContext::with_live_verifier(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new("model_load".into()).unwrap()]).unwrap(),
        resources: List::new(vec![ResourceGrant::Employee { id: EmployeeId([2; 16]) },
            ResourceGrant::Workspace { handle: Text::new("work".into()).unwrap() },
            ResourceGrant::Workspace { handle: Text::new("other".into()).unwrap() }]).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX - 1,
        nonce: [5; 32], jti: [6; 16],
    }, Arc::new(TestLive))
}
struct Resolver { calls: AtomicUsize, changes: bool }
impl ResourceResolver for Resolver {
    fn resolve(&self, context: &RequestContext, _: &ToolPolicy, _: &Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![ResourceGrant::Employee { id: context.record().employee },
            ResourceGrant::Workspace { handle: Text::new(if self.changes && n >= 2 { "other" } else { "work" }.into()).unwrap() }])
    }
}
struct Factory { refused: bool, finishes: AtomicUsize }
impl WorkerPlanFactory for Factory {
    fn supports(&self, tool: &str) -> bool { matches!(tool, "model_load" | "execute_program") }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        Box::pin(async move { if self.refused { Err(VerificationError::Invalid) } else { Ok(()) } })
    }
    fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
        Box::pin(async move { Ok(plan_for(call)) })
    }
    fn finish<'a>(&'a self, _: &'a AuthorizedToolCall, _: WorkerReply) -> PolicyFuture<'a, Value> {
        Box::pin(async move {
            self.finishes.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"content":[{"type":"text","text":"parsed"}],"isError":false}))
        })
    }
}
fn call() -> AuthorizedToolCall {
    tool_policy::authorize(&context(), "model_load", &json!({}),
        &Resolver { calls: AtomicUsize::new(0), changes: false }).unwrap()
}
fn plan() -> WorkerPlan { plan_for(&call()) }
fn plan_for(call: &AuthorizedToolCall) -> WorkerPlan {
    let input = tempfile::tempfile().unwrap();
    let request = table_request(&input, &TableOptions { kind: TableKind::Csv, delimiter: b',',
        has_header: true, offset: 0, limit: 2, sheet: None }).unwrap();
    WorkerPlan::new(request, crate::resources::AuthorizedFile::for_test(call, input)).unwrap()
}
fn reply(request: &ParseRequest) -> WorkerReply {
    WorkerReply { response: ParseResponse { version: request.version, request_id: request.request_id,
        result: ParseResult::Complete { items: List::new(vec![ParsedItem::Text { input_slot: 0,
            page: None, text: Text::new("parsed".into()).unwrap() }]).unwrap() } }, outputs: vec![] }
}
fn bridge(refused: bool, changes: bool, forged: bool) -> (WorkerToolBridge, Arc<Factory>, Arc<AtomicUsize>) {
    let factory = Arc::new(Factory { refused, finishes: AtomicUsize::new(0) });
    let invocations = Arc::new(AtomicUsize::new(0));
    let count = invocations.clone();
    let bridge = WorkerToolBridge::new(factory.clone(),
        Arc::new(Resolver { calls: AtomicUsize::new(0), changes }), Arc::new(move |plan| {
            count.fetch_add(1, Ordering::SeqCst);
            let mut reply = reply(&plan.request);
            if forged { reply.response.request_id = duduclaw_isolation::ids::RequestId([0; 16]); }
            Ok(reply)
        }));
    (bridge, factory, invocations)
}

#[tokio::test]
async fn isolated_worker_factory_serves_only_registered_worker_routes() {
    let (bridge, factory, invocations) = bridge(false, false, false);
    assert!(bridge.supports("model_load"));
    assert!(!bridge.supports("execute_program"));
    assert!(!bridge.supports("model_load_extra"));
    assert_eq!(bridge.execute(&context(), "model_load", &json!({})).await["isError"], false);
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(factory.finishes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn isolated_worker_factory_policy_refusal_never_launches() {
    let (bridge, factory, invocations) = bridge(true, false, false);
    assert_eq!(bridge.execute(&context(), "model_load", &json!({})).await["isError"], true);
    assert_eq!(invocations.load(Ordering::SeqCst), 0);
    assert_eq!(factory.finishes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_worker_factory_rechecks_resource_after_parse() {
    let (bridge, factory, invocations) = bridge(false, true, false);
    assert_eq!(bridge.execute(&context(), "model_load", &json!({})).await["isError"], true);
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(factory.finishes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_worker_factory_rejects_forged_result_before_finish() {
    let (bridge, factory, invocations) = bridge(false, false, true);
    assert_eq!(bridge.execute(&context(), "model_load", &json!({})).await["isError"], true);
    assert_eq!(invocations.load(Ordering::SeqCst), 1);
    assert_eq!(factory.finishes.load(Ordering::SeqCst), 0);
}

#[test]
fn isolated_worker_factory_bounds_plan_and_output_bytes() {
    let mut plan = plan();
    assert!(validate_plan(&plan).is_ok());
    let oversized = tempfile::tempfile().unwrap();
    oversized.set_len(64 * 1024 * 1024 + 1).unwrap();
    plan.input = crate::resources::AuthorizedFile::for_test(&call(), oversized);
    assert!(validate_plan(&plan).is_err());
    plan.input = crate::resources::AuthorizedFile::for_test(&call(), tempfile::tempfile().unwrap());
    plan.request.limits.wall_time_ms = 30_001.try_into().unwrap();
    assert!(validate_plan(&plan).is_err());
    let plan = self::plan();
    let mut result = reply(&plan.request);
    result.outputs.push(vec![1]);
    assert!(validate_reply(&plan.request, &result).is_err());
}

#[test]
fn isolated_worker_factory_checks_output_artifact_digest() {
    let mut request = plan().request;
    request.outputs = List::new(vec![duduclaw_isolation::worker::OutputFd {
        slot: 1, max_bytes: 4.try_into().unwrap(),
    }]).unwrap();
    let bytes = b"data".to_vec();
    let mut result = WorkerReply { response: ParseResponse { version: request.version,
        request_id: request.request_id, result: ParseResult::Complete { items: List::new(vec![
            ParsedItem::OutputArtifact { output_slot: 1, format: duduclaw_isolation::worker::ContentFormat::Text,
                size_bytes: 4, sha256: Sha256Digest(Sha256::digest(&bytes).into()) },
        ]).unwrap() } }, outputs: vec![bytes] };
    assert!(validate_reply(&request, &result).is_ok());
    result.outputs[0][0] = b'x';
    assert!(validate_reply(&request, &result).is_err());
}

#[tokio::test]
async fn isolated_worker_rejects_other_calls_regular_file_before_invoke() {
    struct WrongInput;
    impl WorkerPlanFactory for WrongInput {
        fn supports(&self, _: &str) -> bool { true }
        fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn prepare<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
            Box::pin(async {
                let other = tool_policy::authorize(&context(), "model_load", &json!({}),
                    &Resolver { calls: AtomicUsize::new(2), changes: true })?;
                Ok(plan_for(&other))
            })
        }
        fn finish<'a>(&'a self, _: &'a AuthorizedToolCall, _: WorkerReply) -> PolicyFuture<'a, Value> {
            panic!("another call's input reached finish")
        }
    }
    let bridge = WorkerToolBridge::new(Arc::new(WrongInput),
        Arc::new(Resolver { calls: AtomicUsize::new(0), changes: false }),
        Arc::new(|_| panic!("another call's input reached worker")));
    assert_eq!(bridge.execute(&context(), "model_load", &json!({})).await["isError"], true);
}

struct StatefulFactory;
impl WorkerPlanFactory for StatefulFactory {
    fn supports(&self, name: &str) -> bool { name == "model_load" }
    fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
        Box::pin(async move { Ok(plan_for(call)) })
    }
    fn prepare_with_state<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, (WorkerPlan, Value)> {
        Box::pin(async move { Ok((plan_for(call), call.arguments()["marker"].clone())) })
    }
    fn finish<'a>(&'a self, _: &'a AuthorizedToolCall, _: WorkerReply) -> PolicyFuture<'a, Value> {
        Box::pin(async { panic!("stateful completion must not lose host metadata") })
    }
    fn finish_with_state<'a>(&'a self, _: &'a AuthorizedToolCall, _: WorkerReply, state: Value) -> PolicyFuture<'a, Value> {
        Box::pin(async move { Ok(json!({"marker":state})) })
    }
}

#[tokio::test]
async fn isolated_worker_host_metadata_stays_request_local_across_concurrent_calls() {
    let bridge = WorkerToolBridge::new(Arc::new(StatefulFactory),
        Arc::new(Resolver { calls: AtomicUsize::new(0), changes: false }),
        Arc::new(|plan| Ok(reply(&plan.request))));
    let context = context();
    let first = json!({"marker":"first"});
    let second = json!({"marker":"second"});
    let (a, b) = tokio::join!(bridge.execute(&context, "model_load", &first),
        bridge.execute(&context, "model_load", &second));
    assert_eq!(a["marker"], "first");
    assert_eq!(b["marker"], "second");
}

#[tokio::test]
async fn i1c_composite_keeps_primary_refusal_and_never_falls_through_after_selection() {
    let primary = Arc::new(Factory { refused: true, finishes: AtomicUsize::new(0) });
    let fallback = Arc::new(Factory { refused: false, finishes: AtomicUsize::new(0) });
    let composite = Arc::new(CompositeWorkerPlans::new(vec![primary.clone(), fallback.clone()]).unwrap());
    assert!(composite.supports("model_load"));
    assert!(!composite.supports("missing"));
    assert!(CompositeWorkerPlans::new(vec![]).is_err());
    let bridge = WorkerToolBridge::new(composite, Arc::new(Resolver { calls: AtomicUsize::new(0), changes: false }),
        Arc::new(|_| panic!("primary refusal must stop before E")));
    assert_eq!(bridge.execute(&context(), "model_load", &json!({})).await["isError"], true);
    assert_eq!(primary.finishes.load(Ordering::SeqCst), 0);
    assert_eq!(fallback.finishes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_worker_revoked_before_prepare_has_no_host_effect() {
    struct Revoked;
    impl duduclaw_isolation::capability::LiveRequestVerifier for Revoked {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
            Err(VerificationError::Revoked)
        }
    }
    struct HostEffect(Arc<AtomicUsize>);
    impl WorkerPlanFactory for HostEffect {
        fn supports(&self, _: &str) -> bool { true }
        fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(plan_for(call))
            })
        }
        fn finish<'a>(&'a self, _: &'a AuthorizedToolCall, _: WorkerReply) -> PolicyFuture<'a, Value> {
            panic!("revoked request reached finish")
        }
    }
    let effects = Arc::new(AtomicUsize::new(0));
    let bridge = WorkerToolBridge::new(Arc::new(HostEffect(effects.clone())),
        Arc::new(Resolver { calls: AtomicUsize::new(0), changes: false }),
        Arc::new(|_| panic!("revoked request reached E")));
    let context = RequestContext::with_live_verifier(context().record().clone(), Arc::new(Revoked));
    assert_eq!(bridge.execute(&context, "model_load", &json!({})).await["isError"], true);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolated_worker_prepare_carries_live_authority_across_await() {
    struct Changing(Arc<std::sync::atomic::AtomicBool>);
    impl duduclaw_isolation::capability::LiveRequestVerifier for Changing {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> {
            if self.0.load(Ordering::SeqCst) { Ok(()) } else { Err(VerificationError::Revoked) }
        }
    }
    struct HostEffect { live: Arc<std::sync::atomic::AtomicBool>, effects: Arc<AtomicUsize> }
    impl WorkerPlanFactory for HostEffect {
        fn supports(&self, _: &str) -> bool { true }
        fn authorize_handler<'a>(&'a self, _: &'a AuthorizedToolCall) -> PolicyFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn prepare<'a>(&'a self, call: &'a AuthorizedToolCall) -> PolicyFuture<'a, WorkerPlan> {
            Box::pin(async move {
                assert!(crate::io_authority::is_scoped());
                tokio::task::yield_now().await;
                self.live.store(false, Ordering::SeqCst);
                crate::io_authority::verify_before_io()?;
                self.effects.fetch_add(1, Ordering::SeqCst);
                Ok(plan_for(call))
            })
        }
        fn finish<'a>(&'a self, _: &'a AuthorizedToolCall, _: WorkerReply) -> PolicyFuture<'a, Value> {
            panic!("revoked request reached finish")
        }
    }
    let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let effects = Arc::new(AtomicUsize::new(0));
    let bridge = WorkerToolBridge::new(Arc::new(HostEffect { live: live.clone(), effects: effects.clone() }),
        Arc::new(Resolver { calls: AtomicUsize::new(0), changes: false }),
        Arc::new(|_| panic!("revoked request reached E")));
    let context = RequestContext::with_live_verifier(context().record().clone(), Arc::new(Changing(live)));
    assert_eq!(bridge.execute(&context, "model_load", &json!({})).await["isError"], true);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert!(!crate::io_authority::is_scoped());
}
