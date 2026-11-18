//! One-shot host authorization and protected main-run execution contexts.
//! Ingress must supply authenticated authority; absence is never background.
use std::{future::Future, sync::{Arc, Mutex}, time::Duration};
use duduclaw_isolation::{execution::{ExecutionClass, RuntimeKind, WaitOrigin}, ids::EmployeeId};
use duduclaw_mcp_boundary::{GrantPolicy, resources::ResourceRoots};

pub struct RuntimeAuthorization {
    pub home: std::path::PathBuf,
    pub agent: String,
    pub grant: GrantPolicy,
    pub roots: Arc<ResourceRoots>,
    pub origin: WaitOrigin,
    pub caller: Option<EmployeeId>,
    pub turn: Option<super::HostTurn>,
    pub authenticated_epoch: Option<u64>,
}
impl RuntimeAuthorization {
    /// A task-limited background run must not retain broad employee authority.
    /// Confined IO inputs become exact finite grants; no new service authority
    /// is inferred from the task or from a background/system sender name.
    /// The record `Task` grant (H8) is added; the H10 issuer only narrows to a
    /// task its H9 selector proved the employee owns (otherwise it refuses the
    /// run). `PublicOrigins` and `NewTask` require EmployeeScope (D-H2, H9
    /// decision 3), so they never survive task narrowing.
    pub fn limit_to_task(&mut self, task: String) -> Result<(), super::VerificationError> {
        use duduclaw_isolation::{bounded::{List, Text}, capability::ResourceGrant};
        let task = Text::new(task).map_err(|_| super::VerificationError::Invalid)?;
        let selected = ResourceGrant::Task { id: task.clone() };
        duduclaw_mcp_boundary::tool_policy::validate_resource(&selected)?;
        if self.grant.task.as_ref().is_some_and(|current| current != &task) {
            return Err(super::VerificationError::BindingMismatch);
        }
        let mut resources: Vec<_> = self.grant.resources.as_slice().iter().filter_map(|grant| match grant {
            ResourceGrant::EmployeeScope { .. } | ResourceGrant::PublicOrigins { .. } | ResourceGrant::NewTask { .. } => None,
            ResourceGrant::Confined { resource } => Some(resource.as_ref().clone()),
            _ => Some(grant.clone()),
        }).collect();
        if !resources.contains(&selected) { resources.push(selected); }
        let resources = List::new(resources).map_err(|_| super::VerificationError::Invalid)?;
        self.grant.task = Some(task);
        self.grant.resources = resources;
        Ok(())
    }
}
tokio::task_local! { static AUTHORIZATION: Mutex<Option<RuntimeAuthorization>>; }

/// Only authenticated host ingress/dispatch may call this. It has no serde,
/// environment or RPC constructor. One scope permits one launch, never retries.
pub async fn with_runtime_authorization<F: Future>(authorization: RuntimeAuthorization, future: F) -> F::Output {
    // Nested helpers cannot replace the initiating host's identity or revive a
    // consumed authorization. The outer scope remains the sole authority.
    if AUTHORIZATION.try_with(|_| ()).is_ok() { return future.await; }
    AUTHORIZATION.scope(Mutex::new(Some(authorization)), future).await
}

fn refusal() -> String {
    duduclaw_agent::mcp_spawn_gate::spawn_gate_error(
        "受保護模式尚無這次工作的隔離執行授權，或所選執行設定尚未通過驗證；請由管理員檢查服務狀態。")
}

pub(crate) async fn available(runtime: RuntimeKind) -> bool {
    super::boot::runtime_available(runtime).await
}

/// The writer drains concurrently with runtime output. Its error must be
/// observed before accepting a successful result; drop cancels pending input.
pub(crate) struct PromptWriter {
    task: tokio::task::JoinHandle<Result<(), String>>,
    _principal_context: Option<crate::on_behalf_of::context::ContextGuard>,
}
impl PromptWriter {
    pub(crate) async fn finish(&mut self) -> Result<(), String> {
        (&mut self.task).await.map_err(|_| refusal())?
    }
}
impl Drop for PromptWriter { fn drop(&mut self) { self.task.abort(); } }

async fn write_prompt<W: tokio::io::AsyncWrite + Unpin>(mut stdin: W, bytes: &[u8], deadline: Duration)
    -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    match tokio::time::timeout(deadline, async {
        stdin.write_all(bytes).await?;
        stdin.shutdown().await
    }).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(refusal()),
    }
}

fn take_authorization(agent_dir: &std::path::Path) -> Result<RuntimeAuthorization, String> {
    let authorization = AUTHORIZATION.try_with(|slot| slot.lock().ok().and_then(|mut value| value.take()))
        .ok().flatten().ok_or_else(refusal)?;
    if !authorization.home.is_absolute() || agent_dir != authorization.home.join("agents").join(&authorization.agent) {
        return Err(refusal());
    }
    Ok(authorization)
}

/// R8r-1: the single issuing rule for every protected main run (Claude, the
/// OpenAI-compatible API worker, Codex/Grok and registered CLIs). An
/// authorization installed by [`with_runtime_authorization`] is consumed (one
/// use; a nested run never replaces or revives the outer principal). Without
/// one, authority is issued only from the current legitimate origin: the
/// background record when a background scope exists, otherwise the
/// authenticated interactive ingress. Neither ⇒ refused. There is no
/// synthetic/system origin. Callers run their own coarse gates first and hold
/// the returned principal context until the run has finished.
async fn take_or_issue_current(agent_dir: &std::path::Path)
    -> Result<(RuntimeAuthorization, Option<crate::on_behalf_of::context::ContextGuard>), String> {
    #[cfg(test)]
    if let Ok(fake) = FAKE_ORIGIN.try_with(Arc::clone) {
        return take_or_issue_with(agent_dir, || fake.has_background(),
            |context| std::future::ready(fake.issue("background", agent_dir, context)),
            |context| std::future::ready(fake.issue("ingress", agent_dir, context))).await;
    }
    take_or_issue_with(agent_dir, crate::isolation_background::has_scope,
        |context| crate::isolation_background::authorize_current(agent_dir, context),
        |context| super::ingress::authorize_current(agent_dir, context)).await
}

/// The rule itself, over the two issuers (production: the live background
/// record and ingress scopes; unit tests: fakes through `FAKE_ORIGIN`).
async fn take_or_issue_with<BF, BE, IF, IE>(agent_dir: &std::path::Path,
    background_scope: impl FnOnce() -> bool,
    background: impl FnOnce(Option<String>) -> BF, ingress: impl FnOnce(Option<String>) -> IF)
    -> Result<(RuntimeAuthorization, Option<crate::on_behalf_of::context::ContextGuard>), String>
where BF: Future<Output = Result<RuntimeAuthorization, BE>>, IF: Future<Output = Result<RuntimeAuthorization, IE>> {
    if AUTHORIZATION.try_with(|_| ()).is_ok() {
        return Ok((take_authorization(agent_dir)?, None));
    }
    let agent = agent_dir.file_name().and_then(|name| name.to_str()).ok_or_else(refusal)?;
    let home = agent_dir.parent().and_then(|path| path.parent()).ok_or_else(refusal)?;
    let guard = crate::on_behalf_of::context::register(home, agent).ok_or_else(refusal)?;
    let authorization = if background_scope() {
        background(Some(guard.reference().into())).await.map_err(|_| refusal())?
    } else {
        ingress(Some(guard.reference().into())).await.map_err(|_| refusal())?
    };
    Ok((authorization, Some(guard)))
}

#[cfg(test)]
tokio::task_local! { static FAKE_ORIGIN: Arc<tests::FakeOrigin>; }

/// R6: the step limit root will enforce for this employee's next Claude run.
/// `desired` is the caller's own wish (work turn: the gateway resolution;
/// utility call: at most 50); the result can only be lower. It consumes no
/// authorization. The caller shows exactly this value in the step-budget
/// sentence, uses it to classify a max-turns outcome, and passes it to
/// [`spawn_claude`], which refuses the run if root would enforce anything else.
pub(crate) async fn claude_max_turns(agent_dir: &std::path::Path, desired: u64) -> Result<u16, String> {
    let agent = agent_dir.file_name().and_then(|name| name.to_str()).ok_or_else(refusal)?;
    let home = agent_dir.parent().and_then(|path| path.parent()).ok_or_else(refusal)?;
    match super::boot::claude_max_turns(home.to_path_buf(), agent.to_owned(), desired).await {
        Ok(value) => Ok(value),
        Err(_) => {
            // Agent id and numbers only; no path, policy or entitlement content.
            tracing::warn!(agent, desired, "protected step limit unavailable or outside the root-signed range; run refused");
            Err(refusal())
        }
    }
}

pub(crate) async fn spawn_claude(agent_dir: &std::path::Path, prompt: &str, system_prompt: &str, model: &str, max_turns: u64)
    -> Result<(crate::process_group::BackendChild, PromptWriter), String> {
    let max_turns = u16::try_from(max_turns).map_err(|_| refusal())?;
    let (authorization, principal_context) = take_or_issue_current(agent_dir).await?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::claude_plan(authorization.home.clone(), authorization.agent.clone(), class,
        prompt.as_bytes().to_vec(), system_prompt.as_bytes().to_vec(), model.into(), max_turns).await.map_err(|_| refusal())?;
    spawn_plan(authorization, plan, class, RuntimeKind::Claude, prompt.as_bytes().to_vec(), principal_context).await
}

async fn spawn_plan(authorization: RuntimeAuthorization, plan: duduclaw_isolation::execution::SpawnPolicy,
    class: ExecutionClass, runtime: RuntimeKind, bytes: Vec<u8>,
    principal_context: Option<crate::on_behalf_of::context::ContextGuard>)
    -> Result<(crate::process_group::BackendChild, PromptWriter), String> {
    let launch = super::prepare_launch(super::LaunchInput { agent: authorization.agent, plan,
        grant: authorization.grant, roots: authorization.roots, origin: authorization.origin,
        caller: authorization.caller, turn: authorization.turn, authenticated_epoch: authorization.authenticated_epoch })
        .await.map_err(|_| refusal())?;
    // This builder is only an adapter marker. A cannot execute it locally: the
    // protected latch is required and the root publication selects the binary.
    if !duduclaw_core::execution_gate::isolation_required() { return Err(refusal()); }
    let mut marker = tokio::process::Command::new("root-selected-runtime");
    let admission = crate::autonomous_runtime::admit_current().await?;
    let mut child = duduclaw_execution::command::with_context(launch,
        duduclaw_execution::command::with_runtime(runtime,
            crate::process_group::BackendChild::spawn_class(&mut marker, class))).await
        .map_err(|_| refusal())?;
    crate::autonomous_runtime::mark_handoff();
    drop(admission);
    let stdin = child.child().stdin.take().ok_or_else(refusal)?;
    let control = child.child().control();
    let writer = tokio::spawn(async move {
        let result = write_prompt(stdin, &bytes, Duration::from_secs(30)).await;
        if result.is_err() {
            let _ = control.start_kill();
            return Err(refusal());
        }
        Ok(())
    });
    Ok((child, PromptWriter { task: writer, _principal_context: principal_context }))
}

/// A registered adapter receives one JSON document followed by EOF. Native
/// vendor flags, credentials and legacy configs never enter this launch path.
pub(crate) async fn execute_registered_cli(id: &str, prompt: &str, context: &crate::runtime::RuntimeContext,
    format: duduclaw_core::runtime_catalog::OutputFormat) -> Result<crate::runtime::RuntimeResponse, String> {
    use duduclaw_isolation::bounded::Text;
    // RM-103 S3: refused before any authority is consumed (see
    // `runtime::protected_coarse_native_gate`).
    crate::runtime::protected_coarse_native_gate(context.capabilities.as_ref(), id, &context.agent_id)?;
    let agent_dir = context.agent_dir.as_deref().ok_or_else(refusal)?;
    if context.effort.is_some() || crate::runtime_dispatch::in_utility_call()
        || crate::runtime::spawn_work_dir_override().is_some()
        || agent_dir != context.home_dir.join("agents").join(&context.agent_id) {
        return Err(refusal());
    }
    let runtime = RuntimeKind::Registered(Text::new(format!("generic-cli-v1.{id}")).map_err(|_| refusal())?);
    duduclaw_mcp_boundary::config::registered_cli_arguments(&runtime).map_err(|_| refusal())?;
    // R8r-1: every gate above runs before authority is taken or issued. The
    // principal context stays registered until this run has been collected.
    let (authorization, _principal_context) = take_or_issue_current(agent_dir).await?;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": "generic-cli-v1", "runtime": id, "model": context.model,
        "system_prompt": context.system_prompt, "history": context.conversation_history,
        "prompt": prompt, "max_tokens": context.max_tokens,
    })).map_err(|_| refusal())?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::other_runtime_plan(authorization.home.clone(), authorization.agent.clone(),
        class, runtime.clone(), bytes.clone()).await.map_err(|_| refusal())?;
    let deadline = Duration::from_secs(crate::runtime_config::call_timeout_secs(Some(agent_dir)));
    let out = collect_plan(authorization, plan, class, runtime, bytes, deadline).await?;
    let stdout = std::str::from_utf8(&out).map_err(|_| refusal())?;
    let content = crate::runtime::generic_cli::parse_output(stdout, format).ok_or_else(refusal)?;
    Ok(crate::runtime::RuntimeResponse { partial_timeout_secs: None,
        input_tokens: crate::prompt_compression::estimate_tokens(&crate::runtime::generic_cli::build_prompt(context, prompt, id)),
        output_tokens: crate::prompt_compression::estimate_tokens(&content), content,
        cache_read_tokens: 0, model_used: context.model.clone(), runtime_name: id.into() })
}

async fn collect_plan(authorization: RuntimeAuthorization, plan: duduclaw_isolation::execution::SpawnPolicy,
    class: ExecutionClass, runtime: RuntimeKind, bytes: Vec<u8>, deadline: Duration) -> Result<Vec<u8>, String> {
    let (mut child, mut writer) = spawn_plan(authorization, plan, class, runtime, bytes, None).await?;
    let mut stdout = child.child().stdout.take().ok_or_else(refusal)?;
    let mut stderr = child.child().stderr.take().ok_or_else(refusal)?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(deadline, async {
        tokio::try_join!(writer.finish(),
            async { crate::process_group::drain_bounded(&mut stdout, &mut out, 8 * 1024 * 1024, false)
                .await.map_err(|_| refusal()) },
            async { crate::process_group::drain_bounded(&mut stderr, &mut err, 64 * 1024, true)
                .await.map_err(|_| refusal()) },
            async { child.child().wait().await.map_err(|_| refusal()) })
    }).await;
    match result {
        Ok(Ok(((), false, _, status))) if status.success() => {},
        _ => { child.kill_and_reap().await; return Err(refusal()); }
    };
    child.leader_exited();
    child.disarm();
    Ok(out)
}

/// Native adapters receive only a bounded sealed request. The root publication
/// selects both adapter and vendor ELF; native gateway configs are never used.
pub(crate) async fn execute_native(runtime: RuntimeKind, prompt: String, context: &crate::runtime::RuntimeContext)
    -> Result<Vec<u8>, String> {
    // RM-103 S3: the native write/shell restrictions `execute_native`'s list
    // checks below do not see (`action_rules`, a malformed `[capabilities]`)
    // refuse here, before any authority is consumed.
    let name = match runtime { RuntimeKind::Codex => "codex", RuntimeKind::Grok => "grok", _ => return Err(refusal()) };
    crate::runtime::protected_coarse_native_gate(context.capabilities.as_ref(), name, &context.agent_id)?;
    let agent_dir = context.agent_dir.as_deref().ok_or_else(refusal)?;
    if context.effort.is_some() || crate::runtime_dispatch::in_utility_call()
        || crate::runtime::spawn_work_dir_override().is_some()
        || agent_dir != context.home_dir.join("agents").join(&context.agent_id)
        || context.capabilities.as_ref().is_some_and(|caps| !caps.policy.is_empty()) {
        return Err(refusal());
    }
    if crate::explore_lane::in_explore()
        || context.capabilities.as_ref().is_some_and(|caps| caps.has_tool_restrictions()
            || !caps.scoped_tools.is_empty() || !caps.approval_required_tools.is_empty()
            || !caps.irreversible_tools.is_empty() || !caps.maybe_irreversible_tools.is_empty()) { return Err(refusal()); }
    // PL-0: an externally selected Codex home is not silently replaced by a
    // protected projection. Refuse before consuming credentials or launching.
    if runtime == RuntimeKind::Codex && std::env::var_os("CODEX_HOME").is_some() { return Err(refusal()); }
    let protocol = match runtime { RuntimeKind::Codex => "codex-v1", RuntimeKind::Grok => "grok-v1", _ => return Err(refusal()) };
    // R8r-1: product policy and the interceptor refusals need no authority;
    // they run before it is taken or issued, so a refusal here never consumes
    // a one-time background record or an installed authorization.
    crate::product_policy::require_runtime(&context.home_dir, name).map_err(|_| refusal())?;
    crate::product_policy::require_model(crate::product_policy::load(&context.home_dir).map_err(|_| refusal())?,
        &context.model).map_err(|_| refusal())?;
    if crate::ccr_runtime::for_agent(&context.home_dir, &context.agent_id).is_some()
        || crate::redaction_proxy::try_build_interceptor(&context.home_dir, &context.agent_id,
            &crate::redaction_proxy::current_session_id()).map_err(|_| refusal())?.is_some() { return Err(refusal()); }
    // The principal context stays registered until this run has been collected.
    let (authorization, _principal_context) = take_or_issue_current(agent_dir).await?;
    let bytes = duduclaw_api_worker::native::Request { protocol: protocol.into(), model: context.model.clone(), prompt }
        .encode().map_err(|_| refusal())?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::other_runtime_plan(authorization.home.clone(), authorization.agent.clone(),
        class, runtime.clone(), bytes.clone()).await.map_err(|_| refusal())?;
    let deadline = Duration::from_secs(crate::runtime_config::call_timeout_secs(Some(agent_dir)));
    if runtime != RuntimeKind::Grok { return collect_plan(authorization, plan, class, runtime, bytes, deadline).await; }
    collect_native_pty(authorization, plan, class, deadline).await
}

async fn collect_native_pty(authorization: RuntimeAuthorization, plan: duduclaw_isolation::execution::SpawnPolicy,
    class: ExecutionClass, deadline: Duration) -> Result<Vec<u8>, String> {
    let launch = super::prepare_launch(super::LaunchInput { agent: authorization.agent, plan,
        grant: authorization.grant, roots: authorization.roots, origin: authorization.origin,
        caller: authorization.caller, turn: authorization.turn, authenticated_epoch: authorization.authenticated_epoch })
        .await.map_err(|_| refusal())?;
    if !duduclaw_core::execution_gate::isolation_required() { return Err(refusal()); }
    let marker = portable_pty::CommandBuilder::new("root-selected-runtime");
    let admission = crate::autonomous_runtime::admit_current().await?;
    // I1e supplies protected PTY delivery and resize through this existing A API.
    // Never create a host PTY or retry this one-shot authorization locally.
    let mut child = duduclaw_execution::command::with_context(launch,
        duduclaw_execution::command::with_runtime(RuntimeKind::Grok,
            duduclaw_execution::command::spawn_pty(marker,
                duduclaw_isolation::execution::PtySize { rows: 40, columns: 120 }, class, true)))
        .await.map_err(|_| refusal())?;
    crate::autonomous_runtime::mark_handoff();
    drop(admission);
    // The adapter reads sealed fd 4, not terminal input. No JSON is echoed into
    // the PTY. Dropping this input follows I1e's close/write-half semantics.
    drop(child.input.take());
    let mut stdout = child.output.take().ok_or_else(refusal)?;
    let mut out = Vec::new();
    let result = tokio::time::timeout(deadline, async {
        tokio::try_join!(async { crate::process_group::drain_bounded(&mut stdout, &mut out,
            8 * 1024 * 1024, false).await.map_err(|_| refusal()) },
            async { child.wait().await.map_err(|_| refusal()) })
    }).await;
    match result { Ok(Ok((false, status))) if status.success() => Ok(out),
        _ => { let _ = child.kill().await; Err(refusal()) } }
}

/// Take or issue initiating authority before resolving provider credentials.
/// Unsupported utility, workspace and effort modes never enter the worker's
/// provider path and are refused before any authority is consumed. The caller
/// holds the returned principal context until the worker run has finished.
pub(crate) async fn authorize_api(context: &crate::runtime::RuntimeContext)
    -> Result<(RuntimeAuthorization, Option<crate::on_behalf_of::context::ContextGuard>), String> {
    let agent_dir = context.agent_dir.as_deref().ok_or_else(refusal)?;
    if context.effort.is_some() || crate::runtime_dispatch::in_utility_call()
        || crate::runtime::spawn_work_dir_override().is_some()
        || agent_dir != context.home_dir.join("agents").join(&context.agent_id)
        || context.capabilities.as_ref().is_some_and(|caps| !caps.policy.is_empty()) {
        return Err(refusal());
    }
    take_or_issue_current(agent_dir).await
}

pub(crate) async fn execute_api_worker(authorization: RuntimeAuthorization,
    request: duduclaw_api_worker::Request, context: &crate::runtime::RuntimeContext)
    -> Result<crate::runtime::RuntimeResponse, String> {
    let bytes = request.encode().map_err(|_| refusal())?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::other_runtime_plan(authorization.home.clone(), authorization.agent.clone(),
        class, RuntimeKind::OpenAiCompat, bytes.clone()).await.map_err(|_| refusal())?;
    let out = collect_plan(authorization, plan, class, RuntimeKind::OpenAiCompat, bytes,
        Duration::from_secs(request.timeout_secs)).await?;
    let response: duduclaw_api_worker::Response = serde_json::from_slice(&out).map_err(|_| refusal())?;
    if response.content.trim().is_empty() { return Err(refusal()); }
    Ok(crate::runtime::RuntimeResponse { partial_timeout_secs: None, content: response.content,
        input_tokens: response.input_tokens, output_tokens: response.output_tokens, cache_read_tokens: 0,
        model_used: context.model.clone(), runtime_name: "openai_compat".into() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use duduclaw_isolation::{bounded::{List, Text}, capability::{EmployeeBinding, Lane, Principal}};
    fn authorization(agent: &str) -> RuntimeAuthorization {
        authorization_at(Path::new("/trusted-home"), agent, 1)
    }
    /// `revision` tells the installed (1), ingress (41) and background (42)
    /// authorizations apart in the R8r-1 tests.
    fn authorization_at(home: &Path, agent: &str, revision: u64) -> RuntimeAuthorization {
        RuntimeAuthorization { home: home.into(), agent: agent.into(), grant: GrantPolicy {
            task: None, principal: Principal::Background { tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 },
            binding: EmployeeBinding::Shared(Text::new("tenant".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: revision,
            not_before: 1, expires_at: 2 }, roots: Arc::new(ResourceRoots::default()),
            origin: WaitOrigin::Gateway, caller: None, turn: None, authenticated_epoch: None }
    }
    const INGRESS_REVISION: u64 = 41;
    const BACKGROUND_REVISION: u64 = 42;

    /// R8r-1 seam (installed through `FAKE_ORIGIN`): stands in for the live
    /// background-record and ingress issuers, which read private task-local
    /// scopes of other modules. `background: Some` models a background scope;
    /// its slot is the one-time record. Every issuer call is recorded.
    pub(super) struct FakeOrigin {
        background: Option<Mutex<Option<RuntimeAuthorization>>>,
        ingress: Mutex<Option<RuntimeAuthorization>>,
        calls: Mutex<Vec<(&'static str, std::path::PathBuf, Option<String>)>>,
    }
    impl FakeOrigin {
        fn new(background: Option<Option<RuntimeAuthorization>>, ingress: Option<RuntimeAuthorization>) -> Arc<Self> {
            Arc::new(Self { background: background.map(Mutex::new), ingress: Mutex::new(ingress),
                calls: Mutex::new(Vec::new()) })
        }
        /// Both origins present: the background record and an ingress identity.
        fn both(home: &Path) -> Arc<Self> {
            Self::new(Some(Some(authorization_at(home, "alice", BACKGROUND_REVISION))),
                Some(authorization_at(home, "alice", INGRESS_REVISION)))
        }
        pub(super) fn has_background(&self) -> bool { self.background.is_some() }
        pub(super) fn issue(&self, origin: &'static str, agent_dir: &Path, context: Option<String>)
            -> Result<RuntimeAuthorization, ()> {
            self.calls.lock().unwrap().push((origin, agent_dir.to_owned(), context));
            let slot = if origin == "background" { self.background.as_ref() } else { Some(&self.ingress) };
            slot.and_then(|slot| slot.lock().unwrap().take()).ok_or(())
        }
        fn calls(&self) -> Vec<(&'static str, std::path::PathBuf, Option<String>)> { self.calls.lock().unwrap().clone() }
        fn record_left(&self) -> bool { self.background.as_ref().is_some_and(|slot| slot.lock().unwrap().is_some()) }
        fn ingress_left(&self) -> bool { self.ingress.lock().unwrap().is_some() }
    }

    /// A real, canonical gateway home, so the principal context registers.
    fn live_home() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
        (temp, home)
    }
    fn live_context(home: &Path) -> crate::runtime::RuntimeContext {
        let mut ctx = context();
        ctx.agent_dir = Some(home.join("agents").join("alice"));
        ctx.home_dir = home.to_owned();
        ctx
    }
    /// The four protected paths (Codex only when no external CODEX_HOME,
    /// which PL-0 refuses before any authority).
    fn protected_paths() -> Vec<&'static str> {
        let mut paths = vec!["claude", "api", "grok", "cli"];
        if std::env::var_os("CODEX_HOME").is_none() { paths.push("codex"); }
        paths
    }
    /// Drives one protected path. With authority every path except `api`
    /// then refuses on the missing boot owner, i.e. after the helper.
    /// `max_turns` above the wire range is Claude's coarse refusal.
    async fn run_path(path: &str, ctx: &crate::runtime::RuntimeContext, max_turns: u64) -> Result<(), String> {
        let agent_dir = ctx.agent_dir.clone().unwrap();
        match path {
            "claude" => spawn_claude(&agent_dir, "input", "system", "model", max_turns).await.map(|_| ()),
            "api" => authorize_api(ctx).await.map(|_| ()),
            "codex" => execute_native(RuntimeKind::Codex, "prompt".into(), ctx).await.map(|_| ()),
            "grok" => execute_native(RuntimeKind::Grok, "prompt".into(), ctx).await.map(|_| ()),
            "cli" => execute_registered_cli("qwen", "prompt", ctx,
                duduclaw_core::runtime_catalog::OutputFormat::Text).await.map(|_| ()),
            _ => unreachable!(),
        }
    }
    fn assert_refused(path: &str, result: Result<(), String>) {
        let error = result.expect_err(path);
        assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{path}: {error}");
    }
    fn context() -> crate::runtime::RuntimeContext {
        crate::runtime::RuntimeContext { agent_dir: Some("/trusted-home/agents/alice".into()),
            system_prompt: "system".into(), model: "model".into(), max_tokens: 100,
            home_dir: "/trusted-home".into(), agent_id: "alice".into(), preferred_provider: None,
            conversation_history: vec![], capabilities: None, account_pool: vec![], effort: None,
            allow_cross_family_failover: true }
    }
    /// R8r-1: no installed scope AND no current origin (the live issuers, no
    /// fake: no background scope, no ingress identity) ⇒ every protected path
    /// refuses with the protected-mode refusal before boot, credentials or spawn.
    #[tokio::test]
    async fn every_protected_path_refuses_without_scope_or_origin() {
        let (_temp, home) = live_home();
        let ctx = live_context(&home);
        for path in protected_paths() { assert_refused(path, run_path(path, &ctx, 50).await); }
        assert!(take_or_issue_current(&home.join("agents").join("alice")).await.is_err());
        // The same with the fake seam: the ingress issuer is asked and refuses.
        for path in protected_paths() {
            let fake = FakeOrigin::new(None, None);
            assert_refused(path, FAKE_ORIGIN.scope(fake.clone(), run_path(path, &ctx, 50)).await);
            let calls = fake.calls();
            assert_eq!(calls.len(), 1, "{path}");
            assert_eq!(calls[0].0, "ingress", "{path}");
        }
        // A background scope whose record is already consumed never falls
        // back to the interactive ingress.
        for path in protected_paths() {
            let fake = FakeOrigin::new(Some(None), Some(authorization_at(&home, "alice", INGRESS_REVISION)));
            assert_refused(path, FAKE_ORIGIN.scope(fake.clone(), run_path(path, &ctx, 50)).await);
            assert_eq!(fake.calls().iter().map(|call| call.0).collect::<Vec<_>>(), vec!["background"], "{path}");
            assert!(fake.ingress_left(), "{path}");
        }
    }

    #[tokio::test]
    async fn helper_issues_from_interactive_origin_with_a_live_principal_context() {
        let (_temp, home) = live_home();
        let dir = home.join("agents").join("alice");
        let fake = FakeOrigin::new(None, Some(authorization_at(&home, "alice", INGRESS_REVISION)));
        FAKE_ORIGIN.scope(fake.clone(), async {
            let (auth, guard) = take_or_issue_current(&dir).await.unwrap();
            assert_eq!((auth.agent.as_str(), auth.grant.policy_revision), ("alice", INGRESS_REVISION));
            let guard = guard.expect("an issued run holds its principal context");
            assert_eq!(fake.calls(), vec![("ingress", dir.clone(), Some(guard.reference().to_owned()))]);
            // The ingress identity is one-use.
            assert!(take_or_issue_current(&dir).await.is_err());
        }).await;
    }

    #[tokio::test]
    async fn helper_prefers_background_record_consumes_it_once_and_never_falls_back() {
        let (_temp, home) = live_home();
        let dir = home.join("agents").join("alice");
        let fake = FakeOrigin::both(&home);
        FAKE_ORIGIN.scope(fake.clone(), async {
            let (auth, guard) = take_or_issue_current(&dir).await.unwrap();
            assert_eq!(auth.grant.policy_revision, BACKGROUND_REVISION);
            let guard = guard.expect("an issued run holds its principal context");
            assert_eq!(fake.calls(), vec![("background", dir.clone(), Some(guard.reference().to_owned()))]);
            assert!(!fake.record_left());
            assert!(take_or_issue_current(&dir).await.is_err());
            assert_eq!(fake.calls().iter().map(|call| call.0).collect::<Vec<_>>(), vec!["background", "background"]);
            assert!(fake.ingress_left());
        }).await;
    }

    #[tokio::test]
    async fn helper_consumes_installed_authorization_and_nested_scope_cannot_replace_it() {
        let dir = Path::new("/trusted-home/agents/alice");
        let fake = FakeOrigin::both(Path::new("/trusted-home"));
        FAKE_ORIGIN.scope(fake.clone(), with_runtime_authorization(authorization("alice"), async {
            let (auth, guard) = take_or_issue_current(dir).await.unwrap();
            assert_eq!(auth.grant.policy_revision, 1);
            assert!(guard.is_none());
            with_runtime_authorization(authorization("bob"), async {
                assert!(take_or_issue_current(Path::new("/trusted-home/agents/bob")).await.is_err());
                assert!(take_or_issue_current(dir).await.is_err());
            }).await;
            assert!(take_or_issue_current(dir).await.is_err());
        })).await;
        assert!(fake.calls().is_empty());
        assert!(fake.record_left() && fake.ingress_left());
    }

    /// Behavioural, per path: each of the four paths obtains authority through
    /// the helper exactly as `spawn_claude` does (same issuer, same employee
    /// directory, a registered principal context reference).
    #[tokio::test]
    async fn every_protected_path_issues_from_interactive_origin_like_claude() {
        let (_temp, home) = live_home();
        let ctx = live_context(&home);
        let dir = home.join("agents").join("alice");
        for path in protected_paths() {
            let fake = FakeOrigin::new(None, Some(authorization_at(&home, "alice", INGRESS_REVISION)));
            let result = FAKE_ORIGIN.scope(fake.clone(), run_path(path, &ctx, 50)).await;
            if path == "api" { result.unwrap(); } else { assert_refused(path, result); }
            let calls = fake.calls();
            assert_eq!(calls.len(), 1, "{path}");
            assert_eq!((calls[0].0, &calls[0].1), ("ingress", &dir), "{path}");
            assert!(calls[0].2.as_ref().is_some_and(|reference| reference.len() == 64), "{path}");
            assert!(!fake.ingress_left(), "{path}");
        }
        let fake = FakeOrigin::new(None, Some(authorization_at(&home, "alice", INGRESS_REVISION)));
        FAKE_ORIGIN.scope(fake.clone(), async {
            let (auth, guard) = authorize_api(&ctx).await.unwrap();
            assert_eq!(auth.grant.policy_revision, INGRESS_REVISION);
            assert_eq!(fake.calls()[0].2.as_deref(), guard.as_ref().map(|guard| guard.reference()));
        }).await;
    }

    #[tokio::test]
    async fn every_protected_path_consumes_the_background_record_once() {
        let (_temp, home) = live_home();
        let ctx = live_context(&home);
        let dir = home.join("agents").join("alice");
        for path in protected_paths() {
            let fake = FakeOrigin::both(&home);
            FAKE_ORIGIN.scope(fake.clone(), async {
                let result = run_path(path, &ctx, 50).await;
                if path == "api" { result.unwrap(); } else { assert_refused(path, result); }
                let calls = fake.calls();
                assert_eq!(calls.len(), 1, "{path}");
                assert_eq!((calls[0].0, &calls[0].1), ("background", &dir), "{path}");
                assert!(calls[0].2.is_some(), "{path}");
                assert!(!fake.record_left(), "{path}");
                // Consumed once: a second run of the same path is refused.
                assert_refused(path, run_path(path, &ctx, 50).await);
                assert_eq!(fake.calls().iter().map(|call| call.0).collect::<Vec<_>>(),
                    vec!["background", "background"], "{path}");
                assert!(fake.ingress_left(), "{path}");
            }).await;
        }
        let fake = FakeOrigin::both(&home);
        FAKE_ORIGIN.scope(fake.clone(), async {
            let (auth, _guard) = authorize_api(&ctx).await.unwrap();
            assert_eq!(auth.grant.policy_revision, BACKGROUND_REVISION);
        }).await;
    }

    #[tokio::test]
    async fn every_protected_path_uses_an_installed_authorization_without_issuing() {
        let (_temp, home) = live_home();
        let ctx = live_context(&home);
        let dir = home.join("agents").join("alice");
        for path in protected_paths() {
            let fake = FakeOrigin::both(&home);
            FAKE_ORIGIN.scope(fake.clone(), with_runtime_authorization(authorization_at(&home, "alice", 1), async {
                // A nested scope cannot replace the outer authorization.
                let result = with_runtime_authorization(authorization_at(&home, "alice", 99),
                    run_path(path, &ctx, 50)).await;
                if path == "api" { result.unwrap(); } else { assert_refused(path, result); }
                // The outer authorization was the one consumed; no retry, no issuance.
                assert!(take_authorization(&dir).is_err(), "{path}");
                assert_refused(path, run_path(path, &ctx, 50).await);
            })).await;
            assert!(fake.calls().is_empty(), "{path}");
            assert!(fake.record_left() && fake.ingress_left(), "{path}");
        }
        let fake = FakeOrigin::both(&home);
        FAKE_ORIGIN.scope(fake.clone(), with_runtime_authorization(authorization_at(&home, "alice", 1), async {
            let (auth, guard) = with_runtime_authorization(authorization_at(&home, "alice", 99),
                authorize_api(&ctx)).await.unwrap();
            assert_eq!(auth.grant.policy_revision, 1);
            assert!(guard.is_none());
        })).await;
        assert!(fake.calls().is_empty());
    }

    /// Each path's coarse gate (Claude: step limit outside the wire range;
    /// the others: the context's employee differs from the directory) refuses
    /// before the helper: the background record is still consumable afterwards.
    #[tokio::test]
    async fn coarse_refusal_on_every_path_consumes_no_background_record() {
        let (_temp, home) = live_home();
        let mut bad = live_context(&home);
        bad.agent_id = "bob".into();
        let dir = home.join("agents").join("alice");
        let fake = FakeOrigin::both(&home);
        FAKE_ORIGIN.scope(fake.clone(), async {
            for path in protected_paths() {
                assert_refused(path, run_path(path, &bad, u64::from(u16::MAX) + 1).await);
                assert!(fake.calls().is_empty(), "{path}");
                assert!(fake.record_left(), "{path}");
            }
            let (auth, _guard) = take_or_issue_current(&dir).await.unwrap();
            assert_eq!(auth.grant.policy_revision, BACKGROUND_REVISION);
        }).await;
    }
    /// Runs every protected coarse route (Codex, Grok, a registered generic
    /// CLI) with `caps` inside a live authorization scope and returns the
    /// errors; asserts the one-shot authority was not consumed by the refusal.
    async fn protected_coarse_errors(caps: duduclaw_core::types::CapabilitiesConfig) -> Vec<String> {
        let mut ctx = context();
        ctx.capabilities = Some(caps);
        with_runtime_authorization(authorization("alice"), async {
            let mut errors = Vec::new();
            for kind in [RuntimeKind::Codex, RuntimeKind::Grok] {
                errors.push(execute_native(kind, "prompt".into(), &ctx).await.unwrap_err());
            }
            errors.push(execute_registered_cli("qwen", "prompt", &ctx,
                duduclaw_core::runtime_catalog::OutputFormat::Text).await.unwrap_err());
            // The S3 refusal runs before the authorization is taken.
            assert_eq!(take_authorization(Path::new("/trusted-home/agents/alice")).unwrap().agent, "alice");
            errors
        }).await
    }

    /// RM-103 S3 after the main merge: a native shell restriction (here one
    /// only `action_rules` carries, which the list checks above never saw, and
    /// the fail-closed config of a malformed `[capabilities]`) refuses every
    /// protected coarse runtime — the isolated route is no bypass.
    #[tokio::test]
    async fn protected_mode_still_refuses_shell_constrained_codex_grok_generic() {
        let admin_ask: duduclaw_core::types::CapabilitiesConfig =
            toml::from_str("action_rules = [{ effect = \"admin\", verdict = \"ask\" }]\n").unwrap();
        assert!(admin_ask.native_tool_constraints().needs_no_shell());
        for caps in [admin_ask, crate::runtime::fail_closed_capabilities()] {
            for runtime in ["codex", "grok", "qwen"] {
                assert!(crate::runtime::protected_coarse_native_gate(Some(&caps), runtime, "alice").is_err());
            }
            for error in protected_coarse_errors(caps.clone()).await {
                assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
                assert!(error.contains("cannot enforce") && error.contains("原生指令執行"), "{caps:?}: {error}");
            }
        }
        // Unconstrained configs never get the S3 refusal (they fail later, on
        // the missing boot owner, with the generic protected-mode refusal).
        let ctx = context();
        for kind in [RuntimeKind::Codex, RuntimeKind::Grok] {
            assert!(!execute_native(kind, "prompt".into(), &ctx).await.unwrap_err().contains("cannot enforce"));
        }
        assert!(crate::runtime::protected_coarse_native_gate(
            Some(&duduclaw_core::types::CapabilitiesConfig::default()), "codex", "alice").is_ok());
        assert!(crate::runtime::protected_coarse_native_gate(None, "grok", "alice").is_ok());
    }

    /// RM-103 S3: outside protected mode a write-only constraint narrows the
    /// coarse sandbox to read-only. The isolated profile is root-derived and
    /// has no per-run read-only mode, so in protected mode it is refused.
    #[tokio::test]
    async fn protected_mode_write_constraint_read_only_or_refused() {
        let modify_ask: duduclaw_core::types::CapabilitiesConfig =
            toml::from_str("action_rules = [{ effect = \"modify\", verdict = \"ask\" }]\n").unwrap();
        let n = modify_ask.native_tool_constraints();
        assert!(!n.needs_no_shell() && !n.no_write.is_empty(), "{n:?}");
        // Unprotected Codex: narrowed to the read-only view, not refused.
        assert!(crate::runtime::coarse_native_caps(Some(&modify_ask), "codex", "alice", true).unwrap().is_some());
        for runtime in ["codex", "grok", "qwen"] {
            let error = crate::runtime::protected_coarse_native_gate(Some(&modify_ask), runtime, "alice").unwrap_err();
            assert!(error.contains("原生檔案寫入"), "{runtime}: {error}");
        }
        for error in protected_coarse_errors(modify_ask).await {
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error), "{error}");
            assert!(error.contains("cannot enforce") && error.contains("原生檔案寫入"), "{error}");
        }
    }

    #[tokio::test]
    async fn api_authority_is_one_shot_and_rejects_context_identity_mismatch() {
        with_runtime_authorization(authorization("alice"), async {
            let mut wrong = context(); wrong.agent_id = "bob".into();
            assert!(authorize_api(&wrong).await.is_err());
            let valid = context();
            assert!(authorize_api(&valid).await.is_ok());
            assert!(authorize_api(&valid).await.is_err());
        }).await;
    }
    #[tokio::test]
    async fn missing_runtime_authorization_refuses_before_boot_or_spawn() {
        assert!(spawn_claude(std::path::Path::new("/not-an-agent"), "input", "system", "model", 50).await.is_err());
        // An out-of-wire-range value refuses before any authority is consumed.
        with_runtime_authorization(authorization("alice"), async {
            assert!(spawn_claude(Path::new("/trusted-home/agents/alice"), "input", "system", "model",
                u64::from(u16::MAX) + 1).await.is_err());
            assert_eq!(take_authorization(Path::new("/trusted-home/agents/alice")).unwrap().agent, "alice");
        }).await;
    }
    #[tokio::test]
    async fn step_limit_lookup_refuses_without_boot_owner_and_consumes_no_authority() {
        with_runtime_authorization(authorization("alice"), async {
            let error = claude_max_turns(Path::new("/trusted-home/agents/alice"), 50).await.unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
            assert_eq!(take_authorization(Path::new("/trusted-home/agents/alice")).unwrap().agent, "alice");
        }).await;
        assert!(claude_max_turns(Path::new("/"), 50).await.is_err());
    }
    #[tokio::test]
    async fn runtime_authorization_is_single_use_and_does_not_cross_tasks() {
        with_runtime_authorization(authorization("alice"), async {
            assert!(tokio::spawn(async { take_authorization(Path::new("/trusted-home/agents/alice")).is_err() })
                .await.unwrap());
            assert_eq!(take_authorization(Path::new("/trusted-home/agents/alice")).unwrap().agent, "alice");
            assert!(take_authorization(Path::new("/trusted-home/agents/alice")).is_err());
        }).await;
        assert!(take_authorization(Path::new("/trusted-home/agents/alice")).is_err());
    }
    #[tokio::test]
    async fn nested_scope_cannot_replace_identity_or_revive_consumed_authority() {
        with_runtime_authorization(authorization("alice"), async {
            with_runtime_authorization(authorization("bob"), async {
                assert!(take_authorization(Path::new("/trusted-home/agents/bob")).is_err());
            }).await;
            with_runtime_authorization(authorization("alice"), async {
                assert!(take_authorization(Path::new("/trusted-home/agents/alice")).is_err());
            }).await;
        }).await;
        with_runtime_authorization(authorization("alice"), async {
            assert!(take_authorization(Path::new("/other-home/agents/alice")).is_err());
        }).await;
    }
    #[tokio::test]
    async fn prompt_input_preserves_bytes_and_requires_eof_and_complete_delivery() {
        use tokio::io::AsyncReadExt;
        let (writer, mut reader) = tokio::io::duplex(3);
        let bytes = "system separate; user 繁體\n".as_bytes();
        let mut received = Vec::new();
        let (sent, read) = tokio::join!(write_prompt(writer, bytes, Duration::from_secs(2)),
            reader.read_to_end(&mut received));
        sent.unwrap(); read.unwrap();
        assert_eq!(received, bytes);
        let (writer, reader) = tokio::io::duplex(3);
        drop(reader);
        assert!(write_prompt(writer, bytes, Duration::from_secs(2)).await.is_err());
    }
    #[tokio::test]
    async fn prompt_input_stalled_reader_is_bounded() {
        let (writer, _reader) = tokio::io::duplex(1);
        assert!(write_prompt(writer, b"more than capacity", Duration::from_millis(25)).await.is_err());
    }
}
