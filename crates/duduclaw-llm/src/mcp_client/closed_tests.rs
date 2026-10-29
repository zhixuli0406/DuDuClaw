// Initial source-first declarations. All are NOTCOMPILED / NOTRUN.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn closed_owned_operation_wrong_variant_never_falls_back_or_replays() {
    let guard = Arc::new(FakeClosedGuard::new(true));
    let registry = closed_registry(guard.clone(), std::time::Duration::from_secs(1))
        .await
        .unwrap();
    let outcome = registry.call("closed_tool", json!({"x":1})).await.unwrap();
    assert!(outcome.is_error);
    assert!(outcome.content.contains("unconfirmed"));
    assert_eq!(guard.executed.load(Ordering::SeqCst), 2);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
    assert!(outcome.closed_postprocess.is_some());
}

#[tokio::test]
async fn closed_actual_recipe_mismatch_is_refused_before_owned_execution() {
    let mut fake = FakeClosedGuard::new(false);
    fake.expected_url = "http://127.0.0.1:19002/".into();
    let guard = Arc::new(fake);
    assert!(
        closed_registry(guard.clone(), std::time::Duration::from_secs(1))
            .await
            .is_err()
    );
    assert_eq!(guard.executed.load(Ordering::SeqCst), 0);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn real_loop_closed_global_after_call_cannot_publish_after_original_deadline() {
    let guard = Arc::new(FakeClosedGuard::new(false));
    let registry = closed_registry(guard.clone(), std::time::Duration::from_millis(20))
        .await
        .unwrap();
    let provider = RecordingProvider::new();
    let outcome = crate::run_tool_loop_with_provenance_and_ccr(
        &provider,
        loop_request(),
        &registry,
        2,
        crate::provenance::ProvenanceConfig::default(),
        Some(Arc::new(SlowAfter)),
        None,
    )
    .await
    .unwrap();
    assert!(provider.last_tool_content().contains("unconfirmed"));
    assert!(
        !provider
            .last_tool_content()
            .contains("private-owned-result")
    );
    assert!(
        !provider
            .last_tool_content()
            .contains("private-owner-notice")
    );
    assert!(outcome.ccr_saved_results.is_empty());
    assert_eq!(guard.executed.load(Ordering::SeqCst), 2);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
}

struct CallTiming {
    started: tokio::time::Instant,
    deadline: tokio::time::Instant,
    completed: tokio::time::Instant,
}
struct CallTimingSink {
    calls: Arc<std::sync::Mutex<Vec<CallTiming>>>,
    started: tokio::time::Instant,
    deadline: tokio::time::Instant,
}
struct FakeClosedGuard {
    wrong: bool,
    expected_url: String,
    expected_filter: ToolFilter,
    timeout: std::sync::Mutex<Duration>,
    executed: Arc<AtomicUsize>,
    generic: AtomicUsize,
    body: String,
    clock_step: Duration,
    timings: Option<Arc<std::sync::Mutex<Vec<CallTiming>>>>,
}
impl FakeClosedGuard {
    fn new(wrong: bool) -> Self {
        Self {
            wrong,
            expected_url: "http://127.0.0.1:19001/".into(),
            expected_filter: ToolFilter::default(),
            timeout: std::sync::Mutex::new(Duration::from_secs(1)),
            executed: Arc::new(AtomicUsize::new(0)),
            generic: AtomicUsize::new(0),
            body: "private-owned-result".into(),
            clock_step: Duration::ZERO,
            timings: None,
        }
    }
}
struct FakeOperation {
    discover: bool,
    wrong: bool,
    args: Value,
    count: Arc<AtomicUsize>,
    body: String,
    clock_step: Duration,
    timing: Option<CallTimingSink>,
}
#[async_trait::async_trait]
impl OwnedScopedOperation for FakeOperation {
    async fn execute(self: Box<Self>) -> Result<OwnedScopedReply, OwnedScopedFailure> {
        self.count.fetch_add(1, Ordering::SeqCst);
        if !self.discover && self.args.get("hold_operation") == Some(&json!(true)) {
            std::future::pending::<()>().await;
        }
        if !self.discover && self.args.get("fail_operation") == Some(&json!(true)) {
            return Err(OwnedScopedFailure::Unconfirmed);
        }
        // Only the paused-clock fixture opts in; default operations never yield here.
        if !self.clock_step.is_zero() {
            tokio::time::advance(self.clock_step).await;
        }
        let is_error = self.args.get("isError") == Some(&json!(true));
        let reply = if self.discover || self.wrong {
            OwnedScopedReply::Discover(vec![McpToolDef {
                name: "closed_tool".into(),
                description: "fixture".into(),
                input_schema: json!({"type":"object"}),
            }])
        } else {
            OwnedScopedReply::Tool {
                arguments: self.args,
                result: ToolCallResult {
                    content: self.body,
                    is_error,
                },
                notice: Some("private-owner-notice".into()),
            }
        };
        if let Some(timing) = &self.timing {
            timing.calls.lock().unwrap().push(CallTiming {
                started: timing.started,
                deadline: timing.deadline,
                completed: tokio::time::Instant::now(),
            });
        }
        Ok(reply)
    }
}
#[async_trait::async_trait]
impl ToolInterceptor for FakeClosedGuard {
    fn before_call(&self, _: &str, _: &str, _: Value) -> InterceptDecision {
        self.generic.fetch_add(1, Ordering::SeqCst);
        InterceptDecision::Deny("generic forbidden".into())
    }
    fn after_call(&self, _: &str, _: &str, _: &Value, _: &mut Value) {}
    async fn prepare_closed_client_call(
        &self,
        target: ClosedClientTarget<'_>,
        operation: ClientOperation<'_>,
        args: Value,
    ) -> Result<Box<dyn OwnedScopedOperation + Send + 'static>, String> {
        let expected = ScopedHttpRecipe::new(
            self.expected_url.clone(),
            vec![],
            *self.timeout.lock().unwrap(),
        )
        .unwrap();
        if target.server() != "closed"
            || !target.matches_recipe(&expected)
            || !target.matches_host_binding("owned-fixture-binding")
            || !target.matches_filter(&self.expected_filter)
        {
            return Err("actual slot mismatch".into());
        }
        let discover = matches!(operation, ClientOperation::Discover);
        let timing = if discover {
            None
        } else {
            self.timings.as_ref().map(|calls| CallTimingSink {
                calls: calls.clone(),
                started: tokio::time::Instant::now(),
                deadline: target.deadline(),
            })
        };
        Ok(Box::new(FakeOperation {
            discover,
            wrong: self.wrong,
            args,
            count: self.executed.clone(),
            body: self.body.clone(),
            clock_step: if discover {
                Duration::ZERO
            } else {
                self.clock_step
            },
            timing,
        }))
    }
}
async fn closed_registry(
    guard: Arc<FakeClosedGuard>,
    timeout: Duration,
) -> Result<ToolRegistry, McpError> {
    *guard.timeout.lock().unwrap() = timeout;
    ToolRegistry::from_client_slots_named(
        vec![(
            "closed".into(),
            RegistryClient::scoped_http_closed(
                ScopedHttpRecipe::new("http://127.0.0.1:19001/".into(), vec![], timeout).unwrap(),
                "owned-fixture-binding".into(),
                guard,
            ),
        )],
        vec![],
    )
    .await
}
fn loop_request() -> crate::ChatRequest {
    crate::ChatRequest::new("owned-fixture")
}
struct RecordingProvider {
    seen: std::sync::Mutex<Vec<crate::ChatRequest>>,
    two: bool,
}
impl RecordingProvider {
    fn new() -> Self {
        Self {
            seen: Default::default(),
            two: false,
        }
    }
    fn last_tool_content(&self) -> String {
        self.seen
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .find_map(|p| match p {
                crate::ContentPart::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap()
    }
}
#[async_trait::async_trait]
impl crate::ChatProvider for RecordingProvider {
    fn id(&self) -> &str {
        "owned-fixture"
    }
    async fn complete(
        &self,
        request: &crate::ChatRequest,
    ) -> Result<crate::ChatResponse, crate::LlmError> {
        let mut seen = self.seen.lock().unwrap();
        seen.push(request.clone());
        let first = seen.len() == 1;
        Ok(crate::ChatResponse {
            parts: if first {
                if self.two {
                    vec![
                        crate::ContentPart::ToolCall {
                            id: "closed-call-first".into(),
                            name: "closed_tool".into(),
                            args: json!({}),
                        },
                        crate::ContentPart::ToolCall {
                            id: "closed-call-second".into(),
                            name: "closed_tool".into(),
                            args: json!({}),
                        },
                    ]
                } else {
                    vec![crate::ContentPart::ToolCall {
                        id: "closed-call".into(),
                        name: "closed_tool".into(),
                        args: json!({}),
                    }]
                }
            } else {
                vec![crate::ContentPart::Text("done".into())]
            },
            stop: if first {
                crate::StopReason::ToolUse
            } else {
                crate::StopReason::EndTurn
            },
            usage: Default::default(),
            model_used: "owned-fixture".into(),
            provider: "owned-fixture".into(),
        })
    }
    async fn stream(
        &self,
        _: &crate::ChatRequest,
    ) -> Result<
        futures_util::stream::BoxStream<'static, Result<crate::StreamEvent, crate::LlmError>>,
        crate::LlmError,
    > {
        Err(crate::LlmError::InvalidRequest(
            "unused fixture stream".into(),
        ))
    }
}
struct SlowAfter;
#[async_trait::async_trait]
impl ToolInterceptor for SlowAfter {
    fn before_call(&self, _: &str, _: &str, args: Value) -> InterceptDecision {
        InterceptDecision::Allow(args)
    }
    fn after_call(&self, _: &str, _: &str, _: &Value, _: &mut Value) {
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn ccr_fixture() -> (tempfile::TempDir, crate::CcrRuntime) {
    let directory = tempfile::tempdir().unwrap();
    let scope = crate::CcrScope {
        tenant_id: "m1-tenant".into(),
        agent_id: "m1-agent".into(),
        session_id: "m1-session".into(),
        source_acl: "m1-private".into(),
    };
    let store = crate::CcrStore::new(directory.path().join("ccr.db"));
    store.put(&scope, "seed", "seed", "fixture").unwrap();
    (
        directory,
        crate::CcrRuntime::new(store, scope)
            .restrict_sources(vec![("closed".into(), "closed_tool".into())]),
    )
}
struct DelayedVerifier {
    calls: AtomicUsize,
    delay_on: usize,
}
#[async_trait::async_trait]
impl McpSourceVerifier for DelayedVerifier {
    async fn verify(&self, _: &CcrScope, _: &Value, _: &str) -> Result<VerifiedMcpSource, String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.delay_on {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(VerifiedMcpSource {
            artifact: crate::CcrSourceArtifact {
                connector: "m1-fixture".into(),
                artifact_id: "owned".into(),
                version: "v1".into(),
                acl_revision: "acl1".into(),
            },
            retention_at: 4_000_000_000,
        })
    }
}
#[tokio::test]
async fn closed_same_deadline_withholds_registry_loop_and_postwrite_verifier_results() {
    for delay_on in [1, 2, 3] {
        let mut fake = FakeClosedGuard::new(false);
        fake.body = format!(
            "[{}\"private-owned-result\"{}]",
            " ".repeat(10000),
            " ".repeat(10000)
        );
        let guard = Arc::new(fake);
        let mut registry = closed_registry(guard.clone(), Duration::from_millis(200))
            .await
            .unwrap();
        let (_directory, runtime) = ccr_fixture();
        registry
            .register_source_verifier(
                runtime.scope.clone(),
                "closed",
                "closed_tool",
                Arc::new(DelayedVerifier {
                    calls: AtomicUsize::new(0),
                    delay_on,
                }),
            )
            .unwrap();
        let provider = RecordingProvider::new();
        let outcome = crate::run_tool_loop_with_provenance_and_ccr(
            &provider,
            loop_request(),
            &registry,
            2,
            Default::default(),
            None,
            Some(runtime.clone()),
        )
        .await
        .unwrap();
        assert!(provider.last_tool_content().contains("unconfirmed"));
        assert!(
            !provider
                .last_tool_content()
                .contains("private-owned-result")
        );
        assert!(!provider.last_tool_content().contains("[CCR:"));
        assert!(outcome.ccr_saved_results.is_empty());
        assert!(outcome.ccr_delivery_guards.is_empty());
        assert_eq!(guard.executed.load(Ordering::SeqCst), 2);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if runtime
                    .store
                    .ensure_source_call_active(
                        &runtime.scope,
                        &runtime
                            .source_key_for_call(Some("closed"), "closed_tool")
                            .unwrap(),
                        "closed-call",
                    )
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
}
#[tokio::test]
async fn closed_blocking_ccr_put_can_finish_late_but_never_publish_a_marker() {
    let mut fake = FakeClosedGuard::new(false);
    fake.body = format!(
        "[{}\"private-owned-result\"{}]",
        " ".repeat(10000),
        " ".repeat(10000)
    );
    let guard = Arc::new(fake);
    let registry = closed_registry(guard.clone(), Duration::from_millis(200))
        .await
        .unwrap();
    let (_directory, runtime) = ccr_fixture();
    let lock = rusqlite::Connection::open(runtime.store.path()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let provider = RecordingProvider::new();
    let outcome = crate::run_tool_loop_with_provenance_and_ccr(
        &provider,
        loop_request(),
        &registry,
        2,
        Default::default(),
        None,
        Some(runtime.clone()),
    )
    .await
    .unwrap();
    assert!(provider.last_tool_content().contains("unconfirmed"));
    assert!(!provider.last_tool_content().contains("[CCR:"));
    assert!(outcome.ccr_saved_results.is_empty());
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if runtime
                .store
                .ensure_source_call_active(
                    &runtime.scope,
                    &runtime
                        .source_key_for_call(Some("closed"), "closed_tool")
                        .unwrap(),
                    "closed-call",
                )
                .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(guard.executed.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn cancellation_while_ccr_waits_reconciles_exact_call_without_provider_replay() {
    let mut fake = FakeClosedGuard::new(false);
    fake.body = format!(
        "[{}\"private-owned-result\"{}]",
        " ".repeat(10000),
        " ".repeat(10000)
    );
    let guard = Arc::new(fake);
    let registry = closed_registry(guard.clone(), Duration::from_secs(5))
        .await
        .unwrap();
    let (_directory, runtime) = ccr_fixture();
    let lock = rusqlite::Connection::open(runtime.store.path()).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let provider = RecordingProvider::new();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            crate::run_tool_loop_with_provenance_and_ccr(
                &provider,
                loop_request(),
                &registry,
                2,
                Default::default(),
                None,
                Some(runtime.clone())
            )
        )
        .await
        .is_err()
    );
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if runtime
                .store
                .ensure_source_call_active(
                    &runtime.scope,
                    &runtime
                        .source_key_for_call(Some("closed"), "closed_tool")
                        .unwrap(),
                    "closed-call",
                )
                .is_err()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(guard.executed.load(Ordering::SeqCst), 2);
}
#[test]
fn closed_owned_contracts_are_send_and_static() {
    fn send_static<T: Send + 'static>() {}
    send_static::<Box<dyn OwnedScopedOperation + Send + 'static>>();
}

#[tokio::test]
async fn identical_recipe_with_another_host_entry_binding_cannot_discover() {
    let guard = Arc::new(FakeClosedGuard::new(false));
    let recipe = ScopedHttpRecipe::new(
        "http://127.0.0.1:19001/".into(),
        vec![],
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(
        ToolRegistry::from_client_slots_named(
            vec![(
                "closed".into(),
                RegistryClient::scoped_http_closed(
                    recipe,
                    "another-ordinal-binding".into(),
                    guard.clone()
                )
            )],
            vec![]
        )
        .await
        .is_err()
    );
    assert_eq!(guard.executed.load(Ordering::SeqCst), 0);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn closed_required_missing_source_verifier_refuses_before_preparation_or_effect() {
    let guard = Arc::new(FakeClosedGuard::new(false));
    let mut registry = closed_registry(guard.clone(), Duration::from_secs(1))
        .await
        .unwrap();
    registry.require_source_attestation_for_tool("closed_tool");
    let outcome = registry.call("closed_tool", json!({})).await.unwrap();
    assert!(outcome.is_error);
    assert!(outcome.content.contains("refused"));
    assert_eq!(guard.executed.load(Ordering::SeqCst), 1);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn later_tool_in_batch_cannot_extend_the_earlier_closed_result_deadline() {
    let timings = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut fake = FakeClosedGuard::new(false);
    fake.clock_step = Duration::from_millis(200);
    fake.timings = Some(timings.clone());
    let guard = Arc::new(fake);
    let registry = closed_registry(guard.clone(), Duration::from_millis(300))
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    let provider = RecordingProvider {
        seen: Default::default(),
        two: true,
    };
    let outcome = crate::run_tool_loop_with_provenance_and_ccr(
        &provider,
        loop_request(),
        &registry,
        2,
        Default::default(),
        None,
        None,
    )
    .await
    .unwrap();
    let completed = tokio::time::Instant::now();
    {
        let calls = timings.lock().unwrap();
        assert_eq!(calls.len(), 2, "discovery must not advance or record time");
        assert_eq!(calls[0].started, started);
        assert_eq!(calls[0].deadline, started + Duration::from_millis(300));
        assert_eq!(calls[0].completed, started + Duration::from_millis(200));
        assert_eq!(calls[1].started, calls[0].completed);
        assert_eq!(calls[1].deadline, started + Duration::from_millis(500));
        assert_eq!(calls[1].completed, started + Duration::from_millis(400));
        assert_eq!(completed, calls[1].completed);
        assert!(completed >= calls[0].deadline);
        assert!(completed < calls[1].deadline);
    }
    let seen = provider.seen.lock().unwrap();
    let contents: Vec<_> = seen
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter_map(|p| match p {
            crate::ContentPart::ToolResult { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(contents.len(), 2);
    assert!(contents[0].contains("unconfirmed"));
    assert!(!contents[0].contains("private-owner-notice"));
    assert!(contents[1].contains("private-owned-result"));
    assert!(!outcome.tool_calls[0].success);
    assert!(outcome.tool_calls[1].success);
    assert_eq!(guard.executed.load(Ordering::SeqCst), 3);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn actual_closed_slot_filter_mismatch_refuses_before_owned_execution() {
    let guard = Arc::new(FakeClosedGuard::new(false));
    let recipe = ScopedHttpRecipe::new(
        "http://127.0.0.1:19001/".into(),
        vec![],
        Duration::from_secs(1),
    )
    .unwrap();
    let filter = ToolFilter {
        allowed: vec!["closed_tool".into()],
        denied: vec![],
    };
    assert!(
        ToolRegistry::from_client_slots_named(
            vec![(
                "closed".into(),
                RegistryClient::scoped_http_closed(
                    recipe,
                    "owned-fixture-binding".into(),
                    guard.clone()
                )
            )],
            vec![filter]
        )
        .await
        .is_err()
    );
    assert_eq!(guard.executed.load(Ordering::SeqCst), 0);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
}

// R2 source-first declarations and actual synchronous-boundary probes. NOTRUN.
thread_local! {
    static R2_ADMISSION_PROBE:std::cell::RefCell<Option<R2Probe>>=const {std::cell::RefCell::new(None)};
}
struct R2Probe {
    hook: Box<dyn FnMut(&str, usize)>,
    admitted: Arc<std::sync::Mutex<Vec<(usize, usize, bool)>>>,
}
struct R2ProbeGuard {
    admitted: Arc<std::sync::Mutex<Vec<(usize, usize, bool)>>>,
}
impl R2ProbeGuard {
    fn install(hook: impl FnMut(&str, usize) + 'static) -> Self {
        let admitted = Arc::new(std::sync::Mutex::new(Vec::new()));
        R2_ADMISSION_PROBE.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(R2Probe {
                hook: Box::new(hook),
                admitted: admitted.clone(),
            });
        });
        Self { admitted }
    }
    fn snapshots(&self) -> Vec<(usize, usize, bool)> {
        self.admitted.lock().unwrap().clone()
    }
}
impl Drop for R2ProbeGuard {
    fn drop(&mut self) {
        R2_ADMISSION_PROBE.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
/// Hooks run after actual register/registry preparation, before its last check.
pub(crate) fn r2_admission_checkpoint(stage: &str, spans: usize) {
    R2_ADMISSION_PROBE.with(|slot| {
        if let Some(probe) = slot.borrow_mut().as_mut() {
            (probe.hook)(stage, spans);
        }
    });
}
/// Observer only: no callback after the last admission check.
pub(crate) fn r2_record_admitted(spans: usize, saved: usize, guards: bool) {
    R2_ADMISSION_PROBE.with(|slot| {
        if let Some(probe) = slot.borrow().as_ref() {
            probe.admitted.lock().unwrap().push((spans, saved, guards));
        }
    });
}
#[derive(Debug)]
struct R2SourceValidator;
impl crate::ccr::CcrBoundSourceValidator for R2SourceValidator {
    fn valid(&self, _: &CcrScope, artifact: &crate::CcrSourceArtifact, _: &str) -> bool {
        artifact.connector == "m1-fixture"
            && artifact.artifact_id == "owned"
            && artifact.version == "v1"
            && artifact.acl_revision == "acl1"
    }
}
#[tokio::test]
async fn r2_true_final_ledger_and_prepared_admission_expiry_withhold_all_derived_state() {
    for phase in ["ledger", "ready"] {
        for expire in [false, true] {
            let mut fake = FakeClosedGuard::new(false);
            fake.body = format!(
                "{}{}",
                " ".repeat(100_000),
                json!({"items":vec!["private-owned-result";2000]})
            );
            let guard = Arc::new(fake);
            let mut registry = closed_registry(guard.clone(), Duration::from_secs(1))
                .await
                .unwrap();
            let (_directory, runtime) = ccr_fixture();
            let runtime = runtime.with_bound_source_validator(Arc::new(R2SourceValidator));
            assert!(
                runtime
                    .preview(&guard.body, "00000000-0000-4000-8000-000000000000")
                    .is_some_and(|preview| preview.contains("[CCR:")),
                "phase={phase}, expire={expire}: fixture must reach CCR preview"
            );
            registry
                .register_source_verifier(
                    runtime.scope.clone(),
                    "closed",
                    "closed_tool",
                    Arc::new(DelayedVerifier {
                        calls: AtomicUsize::new(0),
                        delay_on: usize::MAX,
                    }),
                )
                .unwrap();
            let fired = Arc::new(AtomicUsize::new(0));
            let observed = fired.clone();
            let hook = R2ProbeGuard::install(move |stage, spans| {
                if expire
                    && stage == phase
                    && observed
                        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                {
                    if stage == "ledger" {
                        assert_eq!(spans, 1, "phase={phase}, expire={expire}");
                    }
                    std::thread::sleep(Duration::from_millis(1100));
                }
            });
            let provider = RecordingProvider::new();
            let config = crate::provenance::ProvenanceConfig {
                policy: crate::provenance::ProvenancePolicy::Warn,
                initial_ledger: Some(crate::provenance::ProvenanceLedger::new()),
                ..Default::default()
            };
            let outcome = crate::run_tool_loop_with_provenance_and_ccr(
                &provider,
                loop_request(),
                &registry,
                2,
                config,
                None,
                Some(runtime.clone()),
            )
            .await
            .unwrap();
            let content = provider.last_tool_content();
            if expire {
                assert!(content.contains("unconfirmed"), "phase={phase}, expire={expire}");
                assert!(!content.contains("private-owned-result"), "phase={phase}, expire={expire}");
                assert!(!content.contains("private-owner-notice"), "phase={phase}, expire={expire}");
                assert!(!content.contains("[CCR:"), "phase={phase}, expire={expire}");
                assert!(outcome.ccr_saved_results.is_empty(), "phase={phase}, expire={expire}");
                assert!(outcome.ccr_delivery_guards.is_empty(), "phase={phase}, expire={expire}");
                assert!(!outcome.tool_calls[0].success, "phase={phase}, expire={expire}");
                assert!(
                    !outcome.tool_calls[0]
                        .result_text
                        .as_deref()
                        .unwrap()
                        .contains("private-owned-result"),
                    "phase={phase}, expire={expire}"
                );
                assert_eq!(hook.snapshots(), vec![(0, 0, false)], "phase={phase}, expire={expire}");
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        if runtime
                            .store
                            .ensure_source_call_active(
                                &runtime.scope,
                                &runtime
                                    .source_key_for_call(Some("closed"), "closed_tool")
                                    .unwrap(),
                                "closed-call",
                            )
                            .is_err()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
            } else {
                assert!(content.contains("private-owner-notice"), "phase={phase}, expire={expire}");
                assert!(content.contains("[CCR:"), "phase={phase}, expire={expire}");
                assert_eq!(outcome.ccr_saved_results.len(), 1, "phase={phase}, expire={expire}");
                assert!(!outcome.ccr_delivery_guards.is_empty(), "phase={phase}, expire={expire}");
                assert!(outcome.tool_calls[0].success, "phase={phase}, expire={expire}");
                assert_eq!(hook.snapshots(), vec![(1, 1, true)], "phase={phase}, expire={expire}");
            }
            assert_eq!(fired.load(Ordering::SeqCst), if expire { 1 } else { 0 }, "phase={phase}, expire={expire}");
            assert_eq!(guard.executed.load(Ordering::SeqCst), 2, "phase={phase}, expire={expire}");
            assert_eq!(guard.generic.load(Ordering::SeqCst), 0, "phase={phase}, expire={expire}");
        }
    }
}
struct R2StaggerBefore {
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl ToolInterceptor for R2StaggerBefore {
    fn before_call(&self, _: &str, _: &str, args: Value) -> InterceptDecision {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            std::thread::sleep(Duration::from_millis(600));
        }
        InterceptDecision::Allow(args)
    }
    fn after_call(&self, _: &str, _: &str, _: &Value, _: &mut Value) {}
}
#[tokio::test]
async fn r2_later_real_ledger_registration_cannot_expire_an_already_admitted_earlier_call() {
    let guard = Arc::new(FakeClosedGuard::new(false));
    let registry = closed_registry(guard.clone(), Duration::from_secs(1))
        .await
        .unwrap();
    let provider = RecordingProvider {
        seen: Default::default(),
        two: true,
    };
    let fired = Arc::new(AtomicUsize::new(0));
    let observed = fired.clone();
    let mut registrations = 0;
    let hook = R2ProbeGuard::install(move |stage, _| {
        if stage == "ledger" {
            registrations += 1;
            if registrations == 2 {
                observed.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(600));
            }
        }
    });
    let config = crate::provenance::ProvenanceConfig {
        policy: crate::provenance::ProvenancePolicy::Warn,
        initial_ledger: Some(crate::provenance::ProvenanceLedger::new()),
        ..Default::default()
    };
    let outcome = crate::run_tool_loop_with_provenance_and_ccr(
        &provider,
        loop_request(),
        &registry,
        2,
        config,
        Some(Arc::new(R2StaggerBefore {
            calls: AtomicUsize::new(0),
        })),
        None,
    )
    .await
    .unwrap();
    let seen = provider.seen.lock().unwrap();
    let contents: Vec<_> = seen
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter_map(|p| match p {
            crate::ContentPart::ToolResult { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(contents.len(), 2);
    assert!(contents[0].contains("unconfirmed"));
    assert!(!contents[0].contains("private-owner-notice"));
    assert!(contents[1].contains("private-owned-result"));
    assert!(!outcome.tool_calls[0].success);
    assert!(outcome.tool_calls[1].success);
    assert_eq!(hook.snapshots(), vec![(1, 0, false)]);
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    assert_eq!(guard.executed.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn r2_discovery_registry_construction_rechecks_after_the_actual_route_build() {
    for expire in [false, true] {
        let guard = Arc::new(FakeClosedGuard::new(false));
        let hook = R2ProbeGuard::install(move |stage, routes| {
            if stage == "routes" {
                assert_eq!(routes, 1);
                if expire {
                    std::thread::sleep(Duration::from_millis(1100));
                }
            }
        });
        let registry = closed_registry(guard.clone(), Duration::from_secs(1)).await;
        if expire {
            assert!(registry.is_err());
            assert_eq!(guard.executed.load(Ordering::SeqCst), 1);
        } else {
            let registry = registry.unwrap();
            let outcome = registry.call("closed_tool", json!({})).await.unwrap();
            assert!(!outcome.is_error);
            assert_eq!(guard.executed.load(Ordering::SeqCst), 2);
        }
        assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
        drop(hook);
    }
}

#[tokio::test]
async fn additive_boundary_keeps_closed_authority_and_refuses_before_execute() {
    let guard = Arc::new(FakeClosedGuard::new(false));
    let mut registry = closed_registry(guard.clone(), Duration::from_secs(1)).await.unwrap();
    let boundary = Arc::new(super::boundary_tests::Probe::default());
    registry.register_client_boundary(0, boundary.clone()).unwrap();
    assert!(!registry.call("closed_tool", json!({"x":1})).await.unwrap().is_error);
    assert_eq!(*boundary.args.lock().unwrap(), json!({"x":1}));
    assert_eq!(boundary.after.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.released.load(Ordering::SeqCst), 1);
    let executed = guard.executed.load(Ordering::SeqCst);
    boundary.deny.store(true, Ordering::SeqCst);
    assert!(registry.call("closed_tool", json!({"x":2})).await.unwrap().is_error);
    assert_eq!(guard.executed.load(Ordering::SeqCst), executed);
    assert_eq!(guard.generic.load(Ordering::SeqCst), 0);
    assert_eq!(boundary.after.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn egress_closed_error_payload_calls_completion_boundary() {
    let guard = Arc::new(FakeClosedGuard::new(false));
    let mut registry = closed_registry(guard, Duration::from_secs(1)).await.unwrap();
    let boundary = Arc::new(super::boundary_tests::Probe::default());
    registry.register_client_boundary(0, boundary.clone()).unwrap();
    let result = registry.call("closed_tool", json!({"isError":true})).await.unwrap();
    assert!(result.is_error);
    assert!(result.content.contains("private-owned-result"));
    assert_eq!(boundary.after.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn egress_closed_boundary_lease_retains_on_failure_timeout_and_cancel() {
    for case in ["failure", "timeout", "cancel"] {
        let guard = Arc::new(FakeClosedGuard::new(false));
        let timeout = if case == "timeout" { Duration::from_millis(20) } else { Duration::from_secs(5) };
        let mut registry = closed_registry(guard.clone(), timeout).await.unwrap();
        let boundary = Arc::new(super::boundary_tests::Probe::default());
        registry.register_client_boundary(0, boundary.clone()).unwrap();
        if case == "cancel" {
            let registry = Arc::new(registry);
            let task_registry = registry.clone();
            let task = tokio::spawn(async move {
                task_registry.call("closed_tool", json!({"hold_operation":true})).await
            });
            tokio::time::timeout(Duration::from_secs(1), async {
                while guard.executed.load(Ordering::SeqCst) < 2 {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            assert_eq!(boundary.released.load(Ordering::SeqCst), 0, "lease held during execution");
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            let args = if case == "failure" { json!({"fail_operation":true}) } else { json!({"hold_operation":true}) };
            assert!(registry.call("closed_tool", args).await.unwrap().is_error);
        }
        assert_eq!(boundary.released.load(Ordering::SeqCst), 0, "{case}");
        assert_eq!(boundary.after.load(Ordering::SeqCst), 0, "uncertain reads retain their reservation");
    }
}
