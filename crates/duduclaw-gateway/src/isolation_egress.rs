//! In-process egress identity for the connection-bound MCP dispatcher.
//! Missing host turn linkage stays unknown; it never inherits process env.
use std::{future::Future, path::{Path, PathBuf}, sync::Arc};
use duduclaw_mcp_boundary::{RequestContext, VerificationError};

/// Package A supplies the original registered turn for this verified run.
/// Implementations must read the live host mapping, not employee arguments.
pub trait RunTurnResolver: Send + Sync {
    /// Resolve related host fields as one observation. Production registries
    /// override this to reuse a single fresh run/authority lookup.
    fn resolve_run(&self, context: &RequestContext) -> Result<ResolvedRunTurn, VerificationError> {
        Ok(ResolvedRunTurn { identity: self.resolve(context)?, egress_turn: self.egress_turn(context)?, metadata: self.metadata(context)?,
            principal_context: self.principal_context(context)?, utility_runner: self.utility_runner() })
    }

    fn resolve(&self, context: &RequestContext) -> Result<RunTurnIdentity, VerificationError>;

    /// Host-minted execution alias, distinct from the original conversation
    /// turn. Missing linkage stays unknown; it must never reuse provenance.
    fn egress_turn(&self, _context: &RequestContext) -> Result<Option<String>, VerificationError> { Ok(None) }

    /// Read delegation and callback metadata from this run's host-owned row.
    /// Returning no metadata does not identify a root run: nested runs must
    /// never silently reset their depth when a mapping is missing.
    fn metadata(&self, _context: &RequestContext) -> Result<Option<RunMetadata>, VerificationError> {
        Ok(None)
    }
    /// Host execution adapter for utility judges. Some CLI utilities retain
    /// read-capable tools, so they must not run under the gateway uid.
    fn utility_runner(&self) -> Option<std::sync::Arc<dyn UtilityRunner>> { None }

    /// Existing host runtime context retained by A for this exact run. This
    /// preserves attended computer-use proof; never accept a wire reference.
    fn principal_context(&self, _context: &RequestContext) -> Result<Option<String>, VerificationError> {
        Ok(None)
    }

}

/// One host-owned turn snapshot, including its utility execution service.
pub struct ResolvedRunTurn {
    pub identity: RunTurnIdentity,
    pub egress_turn: Option<String>,
    pub metadata: Option<RunMetadata>,
    pub principal_context: Option<String>,
    pub utility_runner: Option<Arc<dyn UtilityRunner>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMetadata {
    pub delegation_depth: u8,
    pub hop_depth: u8,
    pub delegation_origin: Option<String>,
    pub reply_channel: Option<String>,
    pub reply_sender: Option<String>,
    /// Dispatch provenance is an atomic (session, run id) pair.
    pub dispatch_run: Option<(String, String)>,
    /// Triggering channel message is an atomic (sequence, RFC3339 time) pair.
    pub user_message: Option<(i64, String)>,
    pub upstream_unknown: bool,
}

impl RunMetadata {
    /// Only the host's run launcher may identify a fresh, non-delegated run.
    pub fn root() -> Self {
        Self { delegation_depth: 0, hop_depth: 0, delegation_origin: None,
            reply_channel: None, reply_sender: None, dispatch_run: None,
            user_message: None, upstream_unknown: false }
    }

    fn validate(&self) -> Result<(), VerificationError> {
        if self.delegation_origin.as_ref().is_some_and(|origin|
            !duduclaw_core::is_valid_agent_id(origin) || duduclaw_core::is_reserved_agent_id(origin))
            || [&self.reply_channel, &self.reply_sender].into_iter().flatten().any(|value|
                value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)) {
            return Err(VerificationError::Invalid);
        }
        if let Some((session, run)) = &self.dispatch_run {
            duduclaw_memory::SourceRef::other(duduclaw_memory::SourceKind::DispatchRun,
                session.trim(), format!("run:{}", run.trim()), chrono::Utc::now())
                .validate().map_err(|_| VerificationError::Invalid)?;
            if run.trim().is_empty() { return Err(VerificationError::Invalid); }
        }
        if let Some((seq, at)) = &self.user_message {
            if *seq < 0 || chrono::DateTime::parse_from_rfc3339(at.trim()).is_err() {
                return Err(VerificationError::Invalid);
            }
        }
        if self.upstream_unknown && self.dispatch_run.is_none() {
            return Err(VerificationError::Invalid);
        }
        Ok(())
    }
}

/// Original utility request, forwarded without changing provider or policy.
pub struct UtilityRequest<'a> {
    pub home: &'a Path,
    pub agent_dir: Option<&'a Path>,
    pub agent: &'a str,
    pub system_prompt: &'a str,
    pub prompt: &'a str,
    pub max_tokens: u32,
    pub provider: duduclaw_core::types::RuntimeType,
    pub model: &'a str,
    pub hint: Option<&'a crate::runtime_dispatch::UtilityModelHint>,
}

/// A-owned execution service. Run the original utility runtime in an isolated
/// execution context; keep its budget/provider/no-tool policy and collect its
/// result. Implementations must never fall back to a gateway-uid CLI process.
pub trait UtilityRunner: Send + Sync {
    fn run<'a>(&'a self, context: &'a RequestContext, request: UtilityRequest<'a>)
        -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

pub(crate) async fn run_utility(request: UtilityRequest<'_>) -> Option<Result<String, String>> {
    let call = CALL.try_with(Clone::clone).ok()?;
    if call.home != request.home { return Some(Err("isolated utility home mismatch".into())); }
    Some(match call.utility_runner {
        Some(runner) => runner.run(&call.context, request).await,
        None => Err("isolated utility execution backend unavailable".into()),
    })
}

/// Resolve both fields from one live host snapshot. Missing session linkage
/// refuses approval creation; it must not be reconstructed from process env.
pub struct RunTurnIdentity { pub turn: String, pub approval_session: Option<String> }

#[derive(Clone)]
pub(crate) struct CallIdentity {
    context: RequestContext,
    utility_runner: Option<std::sync::Arc<dyn UtilityRunner>>,
    home: PathBuf,
    agent: String,
    pub(crate) turn: String,
    provenance_turn: String,
    approval_session: Option<String>,
    principal_context: String,
    metadata: Option<RunMetadata>,
}

tokio::task_local! { static CALL: CallIdentity; }

pub async fn scope<F: Future>(home: &Path, agent: &str, context: &RequestContext,
    resolver: Option<Arc<dyn RunTurnResolver>>, future: F) -> Result<F::Output, VerificationError> {
    if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent) {
        return Err(VerificationError::Invalid);
    }
    let ResolvedRunTurn { identity, egress_turn, metadata, principal_context: registered, utility_runner } = match resolver {
        Some(resolver) => {
            let context = context.clone();
            duduclaw_mcp_boundary::authority::verify_blocking(move || resolver.resolve_run(&context)).await?
        }
        // Never register this as a fresh clean turn. Unknown lineage is tainted.
        None => ResolvedRunTurn { identity: RunTurnIdentity {
            turn: format!("isolated-run-{}", hex::encode(context.record().run.0)), approval_session: None },
            egress_turn: None, metadata: None, principal_context: None, utility_runner: None },
    };
    let RunTurnIdentity { turn, approval_session } = identity;
    if let Some(metadata) = &metadata { metadata.validate()?; }
    if metadata.as_ref().is_some_and(|value| value.user_message.is_some()) && approval_session.is_none() {
        return Err(VerificationError::Invalid);
    }
    if !crate::computer_use_sessions::turns::valid_turn_id(&turn) {
        return Err(VerificationError::Invalid);
    }
    let egress_turn = match egress_turn {
        Some(alias) if crate::computer_use_sessions::turns::valid_turn_id(&alias) => alias,
        Some(_) => return Err(VerificationError::Invalid),
        None => format!("isolated-run-{}", hex::encode(context.record().run.0)),
    };
    if approval_session.as_ref().is_some_and(|session| session.is_empty() || session.len() > 1024
        || session.chars().any(char::is_control)) { return Err(VerificationError::Invalid); }
    let principal = match &context.record().principal {
        duduclaw_isolation::capability::Principal::Human { user_id, .. } =>
            Some(duduclaw_core::HumanPrincipal::user(user_id.as_str())
                .ok_or(VerificationError::Invalid)?.to_string()),
        duduclaw_isolation::capability::Principal::Background { .. } => None,
    };
    // Existing channel handlers read these gateway task-locals directly. Even
    // absent metadata must shadow the ambient process/caller's callback route.
    let reply_channel = metadata.as_ref().and_then(|value| value.reply_channel.clone()).unwrap_or_default();
    let reply_sender = metadata.as_ref().and_then(|value| value.reply_sender.clone()).unwrap_or_default();
    let (principal_context, _request_guard) = match registered {
        Some(reference) => {
            if crate::on_behalf_of::context::lookup(home, agent, &reference) != Some(principal.clone())
                || crate::on_behalf_of::context::host_task(home, agent, &reference)
                    != context.record().task.as_ref().map(|task| task.as_str().to_owned())
                || crate::on_behalf_of::context::host_run(home, agent, &reference)
                    != metadata.as_ref().and_then(|data| data.dispatch_run.as_ref().map(|(_, run)| run.clone()))
                || crate::on_behalf_of::context::attended_call(home, agent, &reference)
                    .is_some_and(|proof| !proof.matches_turn(agent, &turn)) {
                return Err(VerificationError::BindingMismatch);
            }
            // The runtime owner keeps the guard alive across tool calls and
            // invalidates it on termination. A single request must not revoke it.
            (reference, None)
        }
        None => {
            let guard = crate::on_behalf_of::context::register_isolated(home, agent, context)
                .ok_or(VerificationError::Unavailable)?;
            (guard.reference().to_owned(), Some(guard))
        }
    };
    Ok(crate::on_behalf_of::ON_BEHALF_OF.scope(principal,
        crate::claude_runner::REPLY_CHANNEL.scope(reply_channel,
        crate::claude_runner::REPLY_SENDER.scope(reply_sender,
        CALL.scope(CallIdentity { context: context.clone(), utility_runner, home: home.into(), agent: agent.into(),
            turn: egress_turn, provenance_turn: turn, approval_session,
            principal_context, metadata }, future)))).await)
}

/// Internal helper-policy lookup. Exact verified call binding only; never
/// reconstruct an employee from environment or tool arguments.
pub(crate) fn employee_tool_identity(context: &RequestContext) -> Result<(PathBuf, String), VerificationError> {
    CALL.try_with(|call| {
        let actual = call.context.record().canonical_bytes().map_err(|_| VerificationError::Invalid)?;
        let expected = context.record().canonical_bytes().map_err(|_| VerificationError::Invalid)?;
        if actual != expected { return Err(VerificationError::BindingMismatch); }
        Ok((call.home.clone(), call.agent.clone()))
    }).map_err(|_| VerificationError::Unavailable)?
}

/// Request-local compatibility inputs for existing MCP handlers. No process
/// environment fallback; the guard is revoked when the request future ends.
pub fn resolved_identity() -> Option<(String, Option<String>)> {
    CALL.try_with(|call| (call.provenance_turn.clone(), call.approval_session.clone())).ok()
}

pub fn principal_context() -> Option<String> {
    CALL.try_with(|call| call.principal_context.clone()).ok()
}

pub fn run_metadata() -> Option<RunMetadata> {
    CALL.try_with(|call| call.metadata.clone()).ok().flatten()
}

/// Execution-only identity for the original product's egress clients. Approval
/// and memory consumers must use resolved_identity instead.
pub fn execution_turn() -> Option<String> { CALL.try_with(|call| call.turn.clone()).ok() }

pub(crate) fn turn() -> Option<String> { execution_turn() }

pub(crate) fn approval_origin() -> Option<Result<(String, String), String>> {
    CALL.try_with(|call| call.approval_session.as_ref()
        .map(|session| (session.clone(), call.provenance_turn.clone()))
        .ok_or_else(|| "isolated approval turn origin unavailable".into())).ok()
}

pub(crate) fn current(home: &Path, agent: &str) -> Option<Result<CallIdentity, String>> {
    CALL.try_with(|call| {
        if call.home == home && call.agent == agent { Ok(call.clone()) }
        else { Err("isolated egress identity mismatch".into()) }
    }).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};

    fn context() -> RequestContext {
        RequestContext::from_verified_record(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
            task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
            employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
            gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2, nonce: [5; 32], jti: [6; 16],
        })
    }
    #[derive(Clone)]
    struct Turn(String);
    impl RunTurnResolver for Turn {
        fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
            Ok(RunTurnIdentity { turn: self.0.clone(), approval_session: None })
        }
        fn egress_turn(&self, _: &RequestContext) -> Result<Option<String>, VerificationError> { Ok(Some(self.0.clone())) }
    }

    #[derive(Clone)]
    struct MappedTurn(RunMetadata);
    impl RunTurnResolver for MappedTurn {
        fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
            Ok(RunTurnIdentity { turn: "mapped-run-turn".into(), approval_session: None })
        }
        fn metadata(&self, _: &RequestContext) -> Result<Option<RunMetadata>, VerificationError> {
            Ok(Some(self.0.clone()))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn isolated_turn_snapshot_runs_once_off_the_reactor_thread() {
        struct SnapshotResolver {
            reactor: std::thread::ThreadId,
            calls: std::sync::atomic::AtomicUsize,
        }
        impl RunTurnResolver for SnapshotResolver {
            fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
                panic!("scope must use the combined host snapshot")
            }
            fn resolve_run(&self, _: &RequestContext) -> Result<ResolvedRunTurn, VerificationError> {
                assert_ne!(std::thread::current().id(), self.reactor);
                self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(ResolvedRunTurn { identity: RunTurnIdentity { turn: "host-turn".into(), approval_session: None }, egress_turn: None,
                    metadata: Some(RunMetadata::root()), principal_context: None, utility_runner: None })
            }
        }
        let home = tempfile::tempdir().unwrap();
        let resolver = Arc::new(SnapshotResolver { reactor: std::thread::current().id(),
            calls: std::sync::atomic::AtomicUsize::new(0) });
        scope(home.path(), "main", &context(), Some(resolver.clone()), async {
            assert_eq!(resolved_identity(), Some(("host-turn".into(), None)));
            assert_eq!(run_metadata(), Some(RunMetadata::root()));
        }).await.unwrap();
        assert_eq!(resolver.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn isolated_run_metadata_preserves_root_and_nested_depths() {
        let home = tempfile::tempdir().unwrap();
        let nested = RunMetadata { delegation_depth: 2, hop_depth: 3,
            delegation_origin: Some("manager".into()), reply_channel: Some("telegram:123".into()),
            reply_sender: Some("sender".into()), ..RunMetadata::root() };
        for metadata in [RunMetadata::root(), nested] {
            let resolver = MappedTurn(metadata.clone());
            scope(home.path(), "main", &context(), Some(Arc::new(resolver.clone())), async {
                tokio::task::yield_now().await;
                assert_eq!(crate::claude_runner::reply_sender_for("telegram", "123"), metadata.reply_sender);
                assert_eq!(run_metadata(), Some(metadata));
            }).await.unwrap();
            assert_eq!(run_metadata(), None);
        }
        scope(home.path(), "main", &context(), Some(Arc::new(Turn("missing-metadata".into()))), async {
            assert_eq!(run_metadata(), None);
            assert_eq!(crate::claude_runner::reply_sender_for("telegram", "123"), None);
        }).await.unwrap();
        let bad = MappedTurn(RunMetadata { delegation_origin: Some("../other".into()),
            ..RunMetadata::root() });
        assert!(scope(home.path(), "main", &context(), Some(Arc::new(bad)), async {}).await.is_err());
    }

    #[tokio::test]
    async fn isolated_egress_uses_live_ledger_without_process_credentials() {
        let home = tempfile::tempdir().unwrap();
        let agent = format!("iso-{}", uuid::Uuid::new_v4().simple());
        std::fs::create_dir_all(home.path().join("agents").join(&agent)).unwrap();
        std::fs::write(home.path().join("agents").join(&agent).join("agent.toml"), "").unwrap();
        let turn = Turn(uuid::Uuid::new_v4().to_string());
        crate::egress_guard::register_turn(&agent, &turn.0);
        let read = scope(home.path(), &agent, &context(), Some(Arc::new(turn.clone())), async {
            assert!(!crate::egress_client::query(home.path(), &agent).await.tainted);
            assert!(crate::egress_client::reserve(home.path(), "different", "file_read").await.is_err());
            let (read, _) = crate::egress_client::reserve(home.path(), &agent, "file_read").await.unwrap();
            read
        }).await.unwrap();
        // Completion keeps the captured identity even outside its Tokio scope.
        read.finish(true).await.unwrap();
        let taint = crate::egress_guard::query_turn(&agent, Some(&turn.0));
        assert!(taint.tainted);
        assert!(taint.sources.iter().any(|source| source == "file_read"));
        assert!(super::turn().is_none());
    }

    #[tokio::test]
    async fn isolated_execution_alias_preserves_provenance_approvals_and_monotonic_taint() {
        struct SeparateTurns { original: String, execution: Option<String> }
        impl RunTurnResolver for SeparateTurns {
            fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
                Ok(RunTurnIdentity { turn: self.original.clone(), approval_session: Some("original-session".into()) })
            }
            fn egress_turn(&self, _: &RequestContext) -> Result<Option<String>, VerificationError> {
                Ok(self.execution.clone())
            }
        }
        let home = tempfile::tempdir().unwrap();
        let agent = format!("alias-{}", uuid::Uuid::new_v4().simple());
        std::fs::create_dir_all(home.path().join("agents").join(&agent)).unwrap();
        std::fs::write(home.path().join("agents").join(&agent).join("agent.toml"), "").unwrap();
        let original = uuid::Uuid::new_v4().to_string();
        let execution = format!("runtime-{}", uuid::Uuid::new_v4().simple());
        crate::egress_guard::register_turn(&agent, &execution);
        // An old, unregistered provenance turn must not make a current runtime
        // unknown, and private reads must taint that same runtime across calls.
        for private in [false, true] {
            let resolver = SeparateTurns { original: original.clone(), execution: Some(execution.clone()) };
            scope(home.path(), &agent, &context(), Some(Arc::new(resolver)), async {
                assert_eq!(resolved_identity(), Some((original.clone(), Some("original-session".into()))));
                assert_eq!(approval_origin().unwrap().unwrap(), ("original-session".into(), original.clone()));
                assert_eq!(super::turn(), Some(execution.clone()));
                assert_eq!(crate::egress_client::query(home.path(), &agent).await.tainted, private);
                crate::egress_client::mark(home.path(), &agent, "file_read").await.unwrap();
            }).await.unwrap();
        }
        assert!(crate::egress_guard::query_turn(&agent, Some(&original)).tainted);
        // Missing execution linkage never falls back to even a registered,
        // clean provenance turn. A malformed alias refuses outright.
        crate::egress_guard::register_turn(&agent, &original);
        let resolver = SeparateTurns { original: original.clone(), execution: None };
        scope(home.path(), &agent, &context(), Some(Arc::new(resolver)), async {
            assert!(crate::egress_client::query(home.path(), &agent).await.tainted);
            assert_ne!(super::turn(), Some(original.clone()));
        }).await.unwrap();
        let resolver = SeparateTurns { original, execution: Some("invalid\nalias".into()) };
        assert!(scope(home.path(), &agent, &context(), Some(Arc::new(resolver)), async {}).await.is_err());
    }

    #[tokio::test]
    async fn isolated_egress_missing_or_bad_mapping_never_creates_clean_turn() {
        let home = tempfile::tempdir().unwrap();
        scope(home.path(), "main", &context(), None, async {
            assert!(crate::egress_client::query(home.path(), "main").await.tainted);
            assert!(current(home.path(), "other").unwrap().is_err());
            assert!(approval_origin().unwrap().is_err());
        }).await.unwrap();
        assert!(scope(home.path(), "main", &context(), Some(Arc::new(Turn(String::new()))), async {}).await.is_err());
        assert!(super::turn().is_none());
    }

    #[tokio::test]
    async fn isolated_context_overrides_ambient_principal_and_revokes_reference() {
        let home = tempfile::tempdir().unwrap();
        let reference = crate::on_behalf_of::ON_BEHALF_OF.scope(Some("user:other".into()),
            scope(home.path(), "main", &context(), None, async {
                let reference = principal_context().unwrap();
                assert_eq!(crate::on_behalf_of::current().as_deref(), Some("user:owner"));
                assert_eq!(crate::on_behalf_of::context::lookup(home.path(), "main", &reference),
                    Some(Some("user:owner".into())));
                assert!(crate::on_behalf_of::context::lookup(home.path(), "other", &reference).is_none());
                assert!(crate::on_behalf_of::context::attended_call(home.path(), "main", &reference).is_none());
                reference
            })).await.unwrap();
        assert!(crate::on_behalf_of::context::lookup(home.path(), "main", &reference).is_none());
        assert!(principal_context().is_none());
    }

    #[tokio::test]
    async fn isolated_original_runtime_context_retains_lifetime_and_identity() {
        #[derive(Clone)]
        struct Existing(String);
        impl RunTurnResolver for Existing {
            fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
                Ok(RunTurnIdentity { turn: "host-turn".into(), approval_session: None })
            }
            fn principal_context(&self, _: &RequestContext) -> Result<Option<String>, VerificationError> {
                Ok(Some(self.0.clone()))
            }
        }
        let home = tempfile::tempdir().unwrap();
        let guard = crate::on_behalf_of::ON_BEHALF_OF.scope(Some("user:owner".into()), async {
            crate::on_behalf_of::context::register(home.path(), "main").unwrap()
        }).await;
        let resolver = Existing(guard.reference().into());
        for _ in 0..2 {
            scope(home.path(), "main", &context(), Some(Arc::new(resolver.clone())), async {
                assert_eq!(principal_context().as_deref(), Some(guard.reference()));
            }).await.unwrap();
        }
        assert!(scope(home.path(), "other", &context(), Some(Arc::new(resolver.clone())), async {}).await.is_err());
        let mut other = context().record().clone();
        other.principal = Principal::Human { user_id: Text::new("other".into()).unwrap(), generation: 1 };
        assert!(scope(home.path(), "main", &RequestContext::from_verified_record(other),
            Some(Arc::new(resolver.clone())), async {}).await.is_err());
        drop(guard);
        assert!(scope(home.path(), "main", &context(), Some(Arc::new(resolver.clone())), async {}).await.is_err());
    }

    #[tokio::test]
    async fn isolated_utility_requires_host_runner_and_preserves_request() {
        struct Runner;
        impl UtilityRunner for Runner {
            fn run<'a>(&'a self, context: &'a RequestContext, request: UtilityRequest<'a>)
                -> std::pin::Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
                Box::pin(async move {
                    assert_eq!(context.record().employee, EmployeeId([2; 16]));
                    assert_eq!(request.system_prompt, "system");
                    assert_eq!(request.prompt, "prompt");
                    assert_eq!(request.max_tokens, 1);
                    Ok("original judge result".into())
                })
            }
        }
        struct Resolver;
        impl RunTurnResolver for Resolver {
            fn resolve(&self, _: &RequestContext) -> Result<RunTurnIdentity, VerificationError> {
                Ok(RunTurnIdentity { turn: uuid::Uuid::new_v4().to_string(), approval_session: None })
            }
            fn utility_runner(&self) -> Option<std::sync::Arc<dyn UtilityRunner>> {
                Some(std::sync::Arc::new(Runner))
            }
        }
        let home = tempfile::tempdir().unwrap();
        let request = || UtilityRequest { home: home.path(), agent_dir: None,
            agent: "main", system_prompt: "system", prompt: "prompt", max_tokens: 1, hint: None,
            provider: duduclaw_core::types::RuntimeType::Claude, model: "test-model" };
        assert!(run_utility(request()).await.is_none());
        scope(home.path(), "main", &context(), None, async {
            assert_eq!(run_utility(request()).await,
                Some(Err("isolated utility execution backend unavailable".into())));
        }).await.unwrap();
        scope(home.path(), "main", &context(), Some(Arc::new(Resolver)), async {
            assert_eq!(run_utility(request()).await, Some(Ok("original judge result".into())));
        }).await.unwrap();
        assert!(run_utility(request()).await.is_none());
        // The original provider gate runs before the injected execution service.
        let removed = &duduclaw_core::runtime_catalog::REMOVED_RUNTIMES[0];
        std::fs::write(home.path().join("config.toml"),
            format!("[runtime]\nutility_provider = {:?}\n", removed.id)).unwrap();
        let expected = crate::autonomous_runtime::human_interactive(home.path(),
            crate::runtime_dispatch::run_utility_prompt(home.path(), None, "main", "system", "prompt", 1)).await;
        assert!(expected.as_ref().unwrap_err().contains(removed.removed_in));
        scope(home.path(), "main", &context(), Some(Arc::new(Resolver)), async {
            let actual = crate::autonomous_runtime::human_interactive(home.path(),
                crate::runtime_dispatch::run_utility_prompt(home.path(), None, "main", "system", "prompt", 1)).await;
            assert_eq!(actual, expected);
        }).await.unwrap();
    }
}
