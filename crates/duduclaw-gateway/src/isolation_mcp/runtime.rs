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

/// R8x: what one protected turn did, read by its single logging point
/// ([`observed`]). `site` is the first fixed label recorded (the innermost
/// refusal; an outer mapping of the same failure never overwrites it).
#[derive(Clone, Copy, Default)]
struct Trace { site: Option<RefusalSite>, started: bool, logged: bool }
tokio::task_local! { static TRACE: std::cell::Cell<Trace>; }

use super::refusal::{site, RefusalSite};

/// `refuse!(<label literal>)`: [`refuse`] with a compile-time checked site.
macro_rules! refuse {
    ($label:literal) => { refuse(site!($label)) };
}
/// `keep!(<label literal>)`: [`keep`] with a compile-time checked site.
macro_rules! keep {
    ($label:literal) => { keep(site!($label)) };
}

fn record(update: impl FnOnce(&mut Trace)) {
    let _ = TRACE.try_with(|cell| {
        let mut trace = cell.get();
        update(&mut trace);
        cell.set(trace);
    });
}

/// R8x: record a fixed site (an entry of `refusal::SITES`, typed) for the
/// current protected turn. Outside an observed turn this is a no-op.
pub(crate) fn note_site(site: RefusalSite) {
    record(|trace| if trace.site.is_none() { trace.site = Some(site); });
}

/// The only builder of this module's refusal: the fixed default sentence
/// (`refusal::DEFAULT_MESSAGE`), spawn-gate prefixed, exactly as before R8x,
/// plus the site that refused. It takes a [`RefusalSite`], so a refusal
/// without a site does not compile (a source-scan test rejects any other
/// refusal builder in this file).
fn refuse(site: RefusalSite) -> String {
    note_site(site);
    super::refusal::user_error(None)
}

/// R8x: keep another module's own error text unchanged and record `site`.
pub(crate) fn keep(site: RefusalSite) -> impl FnOnce(String) -> String {
    move |error| {
        note_site(site);
        error
    }
}

/// R8x: the single logging point of a protected turn. Every protected entry
/// (this module's, the API runtime's, and the Codex/Grok/registered-CLI
/// runtimes' protected branches) runs its whole protected turn inside it. A
/// failed turn logs exactly one line: nothing more when the R8t `isolated run
/// refused` line was already written; `isolated run refused after start` when
/// the run started but ended without that line; otherwise `isolated run not
/// started`. All fields are fixed: runtime label, employee label, site label
/// (a forgotten label shows as `unlabelled`, never as a missing line).
/// `runtime` is the fixed `refusal::runtime_label` value.
///
/// Re-entrant: under an outer `observed` scope (same task) an inner one is
/// part of the outer turn — it neither opens a new trace nor logs; the outer
/// scope keeps the one trace and writes the one line.
pub(crate) async fn observed<T, F>(runtime: &'static str, agent: &str, future: F) -> Result<T, String>
where F: Future<Output = Result<T, String>> {
    if TRACE.try_with(|_| ()).is_ok() { return future.await; }
    let (result, trace) = TRACE.scope(std::cell::Cell::new(Trace::default()), async move {
        let result = future.await;
        (result, TRACE.with(|cell| cell.get()))
    }).await;
    if result.is_err() { log_trace(runtime, agent, trace); }
    result
}

/// One WARN per failed protected turn, no local rate limit: the volume is
/// bounded by turn admission upstream, not here — see the log-volume note in
/// `refusal.rs` (above `log_not_started`) for where that admission lives.
fn log_trace(runtime: &'static str, agent: &str, trace: Trace) {
    if trace.logged { return; }
    // No recorded site is passed as `None`; the logger prints `unlabelled`.
    if trace.started { super::refusal::log_refused_after_start(runtime, agent, trace.site); }
    else { super::refusal::log_not_started(runtime, agent, trace.site); }
}

/// R8x: the fixed site of a refused start. A spawnd/transport refusal arrives
/// as an `io::Error` carrying the fixed `BackendError` class (spawnd's wire
/// error codes are folded into it by `duduclaw-execution`); otherwise only the
/// error kind is used. Never the error text.
fn spawn_site(error: &std::io::Error) -> RefusalSite {
    use duduclaw_isolation::execution::BackendError;
    match error.get_ref().and_then(|inner| inner.downcast_ref::<BackendError>()) {
        Some(BackendError::Unavailable) => site!("spawn.backend.unavailable"),
        Some(BackendError::Denied) => site!("spawn.backend.denied"),
        Some(BackendError::WouldDeadlock) => site!("spawn.backend.would_deadlock"),
        Some(BackendError::NotFound) => site!("spawn.backend.not_found"),
        Some(BackendError::Timeout) => site!("spawn.backend.timeout"),
        Some(BackendError::Io) => site!("spawn.backend.io"),
        Some(BackendError::Protocol) => site!("spawn.backend.protocol"),
        None => match error.kind() {
            std::io::ErrorKind::PermissionDenied => site!("spawn.start.permission_denied"),
            std::io::ErrorKind::NotFound => site!("spawn.start.not_found"),
            std::io::ErrorKind::TimedOut => site!("spawn.start.timed_out"),
            _ => site!("spawn.start.other"),
        },
    }
}

/// The employee label used by every protected log line of a turn.
fn agent_of(agent_dir: &std::path::Path) -> &str {
    agent_dir.file_name().and_then(|name| name.to_str()).unwrap_or("")
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
        (&mut self.task).await.map_err(|_| refuse!("run.input"))?
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
        _ => Err(refuse!("run.input")),
    }
}

fn take_authorization(agent_dir: &std::path::Path) -> Result<RuntimeAuthorization, String> {
    let authorization = AUTHORIZATION.try_with(|slot| slot.lock().ok().and_then(|mut value| value.take()))
        .ok().flatten().ok_or_else(|| refuse!("authorization.consumed"))?;
    if !authorization.home.is_absolute() || agent_dir != authorization.home.join("agents").join(&authorization.agent) {
        return Err(refuse!("authorization.installed_mismatch"));
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
    let agent = agent_dir.file_name().and_then(|name| name.to_str())
        .ok_or_else(|| refuse!("authorization.agent_label"))?;
    let home = agent_dir.parent().and_then(|path| path.parent()).ok_or_else(|| refuse!("authorization.home"))?;
    let guard = crate::on_behalf_of::context::register(home, agent)
        .ok_or_else(|| refuse!("authorization.principal_context"))?;
    // The issuers record their own finer `authorization.*` site first.
    let authorization = if background_scope() {
        background(Some(guard.reference().into())).await.map_err(|_| refuse!("authorization.background_issue"))?
    } else {
        ingress(Some(guard.reference().into())).await.map_err(|_| refuse!("authorization.ingress_issue"))?
    };
    Ok((authorization, Some(guard)))
}

#[cfg(test)]
tokio::task_local! { static FAKE_ORIGIN: Arc<tests::FakeOrigin>; }

/// R6: the step limit root will enforce for this employee's next Claude run.
/// `desired` is the caller's own wish (work turn: the gateway resolution;
/// utility call: at most 50); the result can only be lower. It consumes no
/// authorization. Only [`start_claude`] calls it (inside the turn's observed
/// scope): the step-budget sentence and the max-turns classification use
/// exactly this value, and the spawn refuses the run if root would enforce
/// anything else. A refusal is the turn's one `isolated run not started`
/// line with its `step_limit.*` site (no separate warning).
async fn claude_max_turns(agent_dir: &std::path::Path, desired: u64) -> Result<u16, String> {
    let agent = agent_dir.file_name().and_then(|name| name.to_str()).ok_or_else(|| refuse!("step_limit.agent_dir"))?;
    let home = agent_dir.parent().and_then(|path| path.parent()).ok_or_else(|| refuse!("step_limit.agent_dir"))?;
    super::boot::claude_max_turns(home.to_path_buf(), agent.to_owned(), desired).await
        .map_err(|_| refuse!("step_limit.unavailable"))
}

/// R8x (F1): the whole protected Claude turn start — the protected step-limit
/// lookup, the caller's system prompt for exactly the confirmed limit, then
/// authority, plan and spawn — inside ONE observed scope, so every refusal of
/// it (a `step_limit.*` one included) is the turn's one log line with its
/// site. Both production Claude callers (`claude_runner::call_claude_streaming`
/// and `channel_reply::cli_env`) call exactly this; nothing protected runs
/// between their lookup and their spawn outside it. Returns the limit root
/// confirmed with the started run.
pub(crate) async fn start_claude(agent_dir: &std::path::Path, desired: u64, prompt: &str,
    system_prompt: impl FnOnce(u16) -> String, model: &str)
    -> Result<(u16, crate::process_group::BackendChild, PromptWriter), String> {
    observed(super::refusal::runtime_label(&RuntimeKind::Claude), agent_of(agent_dir), async move {
        let max_turns = claude_max_turns(agent_dir, desired).await?;
        let system_prompt = system_prompt(max_turns);
        let (child, writer) = spawn_claude_inner(agent_dir, prompt, &system_prompt, model, u64::from(max_turns)).await?;
        Ok::<_, String>((max_turns, child, writer))
    }).await
}

/// R8x (F1): a protected Claude turn refused by its caller's own option gate
/// (unsupported options, or no employee directory) before [`start_claude`].
/// The refusal ends the turn, so this scope's line is the turn's one line
/// (`preflight.claude_options`); the caller's error text is returned unchanged.
pub(crate) async fn refuse_claude_options(agent_dir: Option<&std::path::Path>, error: String) -> String {
    let agent = agent_dir.map_or("", agent_of);
    observed(super::refusal::runtime_label(&RuntimeKind::Claude), agent,
        async move { Err::<(), String>(keep(site!("preflight.claude_options"))(error)) }).await
        .err().unwrap_or_else(|| refuse!("preflight.claude_options"))
}

/// Tests only: the spawn half of [`start_claude`] with a caller-chosen limit
/// (same inner path, own observed scope), so the per-path authority tests can
/// reach the authority step without a booted root publication.
#[cfg(test)]
pub(crate) async fn spawn_claude(agent_dir: &std::path::Path, prompt: &str, system_prompt: &str, model: &str, max_turns: u64)
    -> Result<(crate::process_group::BackendChild, PromptWriter), String> {
    observed(super::refusal::runtime_label(&RuntimeKind::Claude), agent_of(agent_dir),
        spawn_claude_inner(agent_dir, prompt, system_prompt, model, max_turns)).await
}

async fn spawn_claude_inner(agent_dir: &std::path::Path, prompt: &str, system_prompt: &str, model: &str, max_turns: u64)
    -> Result<(crate::process_group::BackendChild, PromptWriter), String> {
    let max_turns = u16::try_from(max_turns).map_err(|_| refuse!("authorize.max_turns"))?;
    let (authorization, principal_context) = take_or_issue_current(agent_dir).await?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::claude_plan(authorization.home.clone(), authorization.agent.clone(), class,
        prompt.as_bytes().to_vec(), system_prompt.as_bytes().to_vec(), model.into(), max_turns).await
        .map_err(|_| refuse!("plan.unavailable"))?;
    spawn_plan(authorization, plan, class, RuntimeKind::Claude, prompt.as_bytes().to_vec(), principal_context).await
}

async fn spawn_plan(authorization: RuntimeAuthorization, plan: duduclaw_isolation::execution::SpawnPolicy,
    class: ExecutionClass, runtime: RuntimeKind, bytes: Vec<u8>,
    principal_context: Option<crate::on_behalf_of::context::ContextGuard>)
    -> Result<(crate::process_group::BackendChild, PromptWriter), String> {
    // `prepare_launch` records its own finer `launch.*` site first.
    let launch = super::prepare_launch(super::LaunchInput { agent: authorization.agent, plan,
        grant: authorization.grant, roots: authorization.roots, origin: authorization.origin,
        caller: authorization.caller, turn: authorization.turn, authenticated_epoch: authorization.authenticated_epoch })
        .await.map_err(|_| refuse!("spawn.prepare"))?;
    // This builder is only an adapter marker. A cannot execute it locally: the
    // protected latch is required and the root publication selects the binary.
    if !duduclaw_core::execution_gate::isolation_required() { return Err(refuse!("spawn.not_protected")); }
    let mut marker = tokio::process::Command::new("root-selected-runtime");
    let admission = crate::autonomous_runtime::admit_current().await.map_err(keep!("spawn.admission"))?;
    let mut child = duduclaw_execution::command::with_context(launch,
        duduclaw_execution::command::with_runtime(runtime,
            crate::process_group::BackendChild::spawn_class(&mut marker, class))).await
        .map_err(|error| refuse(spawn_site(&error)))?;
    // R8x: from here on the run exists (spawnd accepted StartRun).
    record(|trace| trace.started = true);
    crate::autonomous_runtime::mark_handoff();
    drop(admission);
    let stdin = child.child().stdin.take().ok_or_else(|| refuse!("run.stdin_missing"))?;
    let control = child.child().control();
    let writer = tokio::spawn(async move {
        let result = write_prompt(stdin, &bytes, Duration::from_secs(30)).await;
        if result.is_err() {
            let _ = control.start_kill();
            return Err(refuse!("run.input"));
        }
        Ok(())
    });
    Ok((child, PromptWriter { task: writer, _principal_context: principal_context }))
}

/// A registered adapter receives one JSON document followed by EOF. Native
/// vendor flags, credentials and legacy configs never enter this launch path.
/// R8x: the coarse gates shared by the protected non-Claude paths, each with
/// its own site (same conditions and the same single refusal as before).
/// `policy`: the `[capabilities] policy` gate (API worker and native paths).
fn coarse_gates(context: &crate::runtime::RuntimeContext, policy: bool) -> Result<&std::path::Path, String> {
    let agent_dir = context.agent_dir.as_deref().ok_or_else(|| refuse!("authorize.agent_dir_missing"))?;
    if context.effort.is_some() { return Err(refuse!("authorize.effort")); }
    if crate::runtime_dispatch::in_utility_call() { return Err(refuse!("authorize.utility_call")); }
    if crate::runtime::spawn_work_dir_override().is_some() { return Err(refuse!("authorize.work_dir_override")); }
    if agent_dir != context.home_dir.join("agents").join(&context.agent_id) { return Err(refuse!("authorize.agent_dir")); }
    if policy && context.capabilities.as_ref().is_some_and(|caps| !caps.policy.is_empty()) {
        return Err(refuse!("authorize.capabilities_policy"));
    }
    Ok(agent_dir)
}

/// The `runtime` log value of a registered CLI (`refusal::runtime_label`).
pub(crate) const REGISTERED_LABEL: &str = "registered";

/// A registered adapter receives one JSON document followed by EOF. Native
/// vendor flags, credentials and legacy configs never enter this launch path.
/// R8x: observed (one log line per refused turn); the runtime's protected
/// branch (`runtime::generic_cli`) wraps its own preflight and this call in
/// an outer observed scope, which this one joins.
pub(crate) async fn execute_registered_cli(id: &str, prompt: &str, context: &crate::runtime::RuntimeContext,
    format: duduclaw_core::runtime_catalog::OutputFormat) -> Result<crate::runtime::RuntimeResponse, String> {
    observed(REGISTERED_LABEL, &context.agent_id, execute_registered_cli_inner(id, prompt, context, format)).await
}

async fn execute_registered_cli_inner(id: &str, prompt: &str, context: &crate::runtime::RuntimeContext,
    format: duduclaw_core::runtime_catalog::OutputFormat) -> Result<crate::runtime::RuntimeResponse, String> {
    use duduclaw_isolation::bounded::Text;
    // RM-103 S3: refused before any authority is consumed (see
    // `runtime::protected_coarse_native_gate`).
    crate::runtime::protected_coarse_native_gate(context.capabilities.as_ref(), id, &context.agent_id)
        .map_err(keep!("authorize.native_gate"))?;
    let agent_dir = coarse_gates(context, false)?;
    let runtime = RuntimeKind::Registered(Text::new(format!("generic-cli-v1.{id}"))
        .map_err(|_| refuse!("authorize.runtime_id"))?);
    duduclaw_mcp_boundary::config::registered_cli_arguments(&runtime).map_err(|_| refuse!("authorize.runtime_id"))?;
    // R8r-1: every gate above runs before authority is taken or issued. The
    // principal context stays registered until this run has been collected.
    let (authorization, _principal_context) = take_or_issue_current(agent_dir).await?;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": "generic-cli-v1", "runtime": id, "model": context.model,
        "system_prompt": context.system_prompt, "history": context.conversation_history,
        "prompt": prompt, "max_tokens": context.max_tokens,
    })).map_err(|_| refuse!("request.encode"))?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::other_runtime_plan(authorization.home.clone(), authorization.agent.clone(),
        class, runtime.clone(), bytes.clone()).await.map_err(|_| refuse!("plan.unavailable"))?;
    let deadline = Duration::from_secs(crate::runtime_config::call_timeout_secs(Some(agent_dir)));
    let out = collect_plan(authorization, plan, class, runtime, bytes, deadline, false).await?;
    let stdout = std::str::from_utf8(&out).map_err(|_| refuse!("output.utf8"))?;
    let content = crate::runtime::generic_cli::parse_output(stdout, format).ok_or_else(|| refuse!("output.parse"))?;
    Ok(crate::runtime::RuntimeResponse { partial_timeout_secs: None,
        input_tokens: crate::prompt_compression::estimate_tokens(&crate::runtime::generic_cli::build_prompt(context, prompt, id)),
        output_tokens: crate::prompt_compression::estimate_tokens(&content), content,
        cache_read_tokens: 0, model_used: context.model.clone(), runtime_name: id.into() })
}

/// R8t: a failed run logs one `isolated run refused` warning (fixed fields,
/// never stderr bytes). Only the API worker (`api_reason`) has its stderr
/// matched against the fixed refusal reasons, which then select the class
/// message; the other runtimes log `reason="unrecognised"` and keep the
/// default refusal. The stderr buffer is dropped before returning.
async fn collect_plan(authorization: RuntimeAuthorization, plan: duduclaw_isolation::execution::SpawnPolicy,
    class: ExecutionClass, runtime: RuntimeKind, bytes: Vec<u8>, deadline: Duration, api_reason: bool)
    -> Result<Vec<u8>, String> {
    use super::refusal::RunExit;
    let agent = authorization.agent.clone();
    let label = super::refusal::runtime_label(&runtime);
    let (mut child, mut writer) = spawn_plan(authorization, plan, class, runtime, bytes, None).await?;
    let mut stdout = child.child().stdout.take().ok_or_else(|| refuse!("run.stdout_missing"))?;
    let mut stderr = child.child().stderr.take().ok_or_else(|| refuse!("run.stderr_missing"))?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(deadline, async {
        tokio::try_join!(writer.finish(),
            async { crate::process_group::drain_bounded(&mut stdout, &mut out, 8 * 1024 * 1024, false)
                .await.map_err(|_| refuse!("run.output_drain")) },
            async { crate::process_group::drain_bounded(&mut stderr, &mut err, 64 * 1024, true)
                .await.map_err(|_| refuse!("run.stderr_drain")) },
            async { child.child().wait().await.map_err(|_| refuse!("run.wait")) })
    }).await;
    let exit = match &result {
        Ok(Ok(((), false, _, status))) if status.success() => None,
        Ok(Ok((_, _, _, status))) => Some(RunExit::from_status(*status)),
        Ok(Err(_)) => Some(RunExit::Incomplete),
        Err(_) => Some(RunExit::Timeout),
    };
    if let Some(exit) = exit {
        child.kill_and_reap().await;
        let reason = if api_reason { super::refusal::refusal_reason(&err) } else { None };
        super::refusal::log_refused(label, &agent, reason, exit, err.len());
        // R8x: the R8t line is this turn's one line.
        record(|trace| trace.logged = true);
        drop(err);
        note_site(site!("run.refused"));
        return Err(if api_reason { super::refusal::user_error(reason.and_then(super::refusal::class_of)) }
            else { refuse!("run.refused") });
    }
    child.leader_exited();
    child.disarm();
    Ok(out)
}

/// Native adapters receive only a bounded sealed request. The root publication
/// selects both adapter and vendor ELF; native gateway configs are never used.
/// R8x: observed (one log line per refused turn); the Codex/Grok runtimes'
/// protected branches wrap their preflight, this call and their output checks
/// in an outer observed scope, which this one joins.
pub(crate) async fn execute_native(runtime: RuntimeKind, prompt: String, context: &crate::runtime::RuntimeContext)
    -> Result<Vec<u8>, String> {
    let label = super::refusal::runtime_label(&runtime);
    observed(label, &context.agent_id, execute_native_inner(runtime, prompt, context)).await
}

async fn execute_native_inner(runtime: RuntimeKind, prompt: String, context: &crate::runtime::RuntimeContext)
    -> Result<Vec<u8>, String> {
    // RM-103 S3: the native write/shell restrictions `execute_native`'s list
    // checks below do not see (`action_rules`, a malformed `[capabilities]`)
    // refuse here, before any authority is consumed.
    let name = match runtime { RuntimeKind::Codex => "codex", RuntimeKind::Grok => "grok",
        _ => return Err(refuse!("authorize.runtime_kind")) };
    crate::runtime::protected_coarse_native_gate(context.capabilities.as_ref(), name, &context.agent_id)
        .map_err(keep!("authorize.native_gate"))?;
    let agent_dir = coarse_gates(context, true)?;
    if crate::explore_lane::in_explore() { return Err(refuse!("authorize.explore_lane")); }
    if context.capabilities.as_ref().is_some_and(|caps| caps.has_tool_restrictions()
            || !caps.scoped_tools.is_empty() || !caps.approval_required_tools.is_empty()
            || !caps.irreversible_tools.is_empty() || !caps.maybe_irreversible_tools.is_empty()) {
        return Err(refuse!("authorize.tool_restrictions"));
    }
    // PL-0: an externally selected Codex home is not silently replaced by a
    // protected projection. Refuse before consuming credentials or launching.
    if runtime == RuntimeKind::Codex && std::env::var_os("CODEX_HOME").is_some() {
        return Err(refuse!("authorize.codex_home"));
    }
    let protocol = match runtime { RuntimeKind::Codex => "codex-v1", RuntimeKind::Grok => "grok-v1",
        _ => return Err(refuse!("authorize.runtime_kind")) };
    // R8r-1: product policy and the interceptor refusals need no authority;
    // they run before it is taken or issued, so a refusal here never consumes
    // a one-time background record or an installed authorization.
    crate::product_policy::require_runtime(&context.home_dir, name).map_err(|_| refuse!("authorize.product_policy"))?;
    crate::product_policy::require_model(crate::product_policy::load(&context.home_dir)
        .map_err(|_| refuse!("authorize.product_policy"))?, &context.model)
        .map_err(|_| refuse!("authorize.product_policy"))?;
    if crate::ccr_runtime::for_agent(&context.home_dir, &context.agent_id).is_some()
        || crate::redaction_proxy::try_build_interceptor(&context.home_dir, &context.agent_id,
            &crate::redaction_proxy::current_session_id()).map_err(|_| refuse!("authorize.interceptor"))?.is_some() {
        return Err(refuse!("authorize.interceptor"));
    }
    // The principal context stays registered until this run has been collected.
    let (authorization, _principal_context) = take_or_issue_current(agent_dir).await?;
    let bytes = duduclaw_api_worker::native::Request { protocol: protocol.into(), model: context.model.clone(), prompt }
        .encode().map_err(|_| refuse!("request.encode"))?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::other_runtime_plan(authorization.home.clone(), authorization.agent.clone(),
        class, runtime.clone(), bytes.clone()).await.map_err(|_| refuse!("plan.unavailable"))?;
    let deadline = Duration::from_secs(crate::runtime_config::call_timeout_secs(Some(agent_dir)));
    if runtime != RuntimeKind::Grok { return collect_plan(authorization, plan, class, runtime, bytes, deadline, false).await; }
    collect_native_pty(authorization, plan, class, deadline).await
}

async fn collect_native_pty(authorization: RuntimeAuthorization, plan: duduclaw_isolation::execution::SpawnPolicy,
    class: ExecutionClass, deadline: Duration) -> Result<Vec<u8>, String> {
    // R8t: for the `isolated run refused` log line only (PTY: no separate stderr).
    let agent = authorization.agent.clone();
    let launch = super::prepare_launch(super::LaunchInput { agent: authorization.agent, plan,
        grant: authorization.grant, roots: authorization.roots, origin: authorization.origin,
        caller: authorization.caller, turn: authorization.turn, authenticated_epoch: authorization.authenticated_epoch })
        .await.map_err(|_| refuse!("spawn.prepare"))?;
    if !duduclaw_core::execution_gate::isolation_required() { return Err(refuse!("spawn.not_protected")); }
    let marker = portable_pty::CommandBuilder::new("root-selected-runtime");
    let admission = crate::autonomous_runtime::admit_current().await.map_err(keep!("spawn.admission"))?;
    // I1e supplies protected PTY delivery and resize through this existing A API.
    // Never create a host PTY or retry this one-shot authorization locally.
    let mut child = duduclaw_execution::command::with_context(launch,
        duduclaw_execution::command::with_runtime(RuntimeKind::Grok,
            duduclaw_execution::command::spawn_pty(marker,
                duduclaw_isolation::execution::PtySize { rows: 40, columns: 120 }, class, true)))
        .await.map_err(|error| refuse(spawn_site(&error)))?;
    // R8x: from here on the run exists.
    record(|trace| trace.started = true);
    crate::autonomous_runtime::mark_handoff();
    drop(admission);
    // The adapter reads sealed fd 4, not terminal input. No JSON is echoed into
    // the PTY. Dropping this input follows I1e's close/write-half semantics.
    drop(child.input.take());
    let mut stdout = child.output.take().ok_or_else(|| refuse!("run.stdout_missing"))?;
    let mut out = Vec::new();
    let result = tokio::time::timeout(deadline, async {
        tokio::try_join!(async { crate::process_group::drain_bounded(&mut stdout, &mut out,
            8 * 1024 * 1024, false).await.map_err(|_| refuse!("run.output_drain")) },
            async { child.wait().await.map_err(|_| refuse!("run.wait")) })
    }).await;
    use super::refusal::RunExit;
    let exit = match &result {
        Ok(Ok((false, status))) if status.success() => return Ok(out),
        Ok(Ok((_, status))) => RunExit::from_status(*status),
        Ok(Err(_)) => RunExit::Incomplete,
        Err(_) => RunExit::Timeout,
    };
    let _ = child.kill().await;
    super::refusal::log_refused(super::refusal::runtime_label(&RuntimeKind::Grok), &agent, None, exit, 0);
    // R8x: the R8t line is this turn's one line.
    record(|trace| trace.logged = true);
    Err(refuse!("run.refused"))
}

/// Take or issue initiating authority before resolving provider credentials.
/// Unsupported utility, workspace and effort modes never enter the worker's
/// provider path and are refused before any authority is consumed. The caller
/// holds the returned principal context until the worker run has finished.
/// R8x: call inside [`observed`] (the API runtime's protected turn does).
pub(crate) async fn authorize_api(context: &crate::runtime::RuntimeContext)
    -> Result<(RuntimeAuthorization, Option<crate::on_behalf_of::context::ContextGuard>), String> {
    let agent_dir = coarse_gates(context, true)?;
    take_or_issue_current(agent_dir).await
}

/// R8x: call inside [`observed`] (the API runtime's protected turn does).
pub(crate) async fn execute_api_worker(authorization: RuntimeAuthorization,
    request: duduclaw_api_worker::Request, context: &crate::runtime::RuntimeContext)
    -> Result<crate::runtime::RuntimeResponse, String> {
    let bytes = request.encode().map_err(|_| refuse!("request.encode"))?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    // `other_runtime_plan` records its own finer `plan.*` site first.
    let plan = super::boot::other_runtime_plan(authorization.home.clone(), authorization.agent.clone(),
        class, RuntimeKind::OpenAiCompat, bytes.clone()).await.map_err(|_| refuse!("plan.unavailable"))?;
    // R8t: a refusal names the worker's fixed reason in the log and selects
    // the class message (still spawn-gate prefixed: no rotation/fallback).
    let out = collect_plan(authorization, plan, class, RuntimeKind::OpenAiCompat, bytes,
        Duration::from_secs(request.timeout_secs), true).await?;
    let response: duduclaw_api_worker::Response = serde_json::from_slice(&out).map_err(|_| refuse!("output.parse"))?;
    if response.content.trim().is_empty() { return Err(refuse!("output.empty")); }
    Ok(crate::runtime::RuntimeResponse { partial_timeout_secs: None, content: response.content,
        input_tokens: response.input_tokens, output_tokens: response.output_tokens, cache_read_tokens: 0,
        model_used: context.model.clone(), runtime_name: "openai_compat".into() })
}

#[cfg(test)]
pub(super) mod tests {
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

    // ── R8x: refusal sites ──────────────────────────────────────────────

    const LABEL_PREFIXES: [&str; 10] = ["preflight.", "authorize.", "step_limit.", "authorization.", "request.",
        "plan.", "launch.", "spawn.", "run.", "output."];

    /// Runs `future` inside a fresh turn carrier; returns the site it recorded.
    pub(crate) async fn site_of(future: impl Future<Output = ()>) -> Option<&'static str> {
        TRACE.scope(std::cell::Cell::new(Trace::default()), async {
            future.await;
            TRACE.with(|cell| cell.get()).site.map(RefusalSite::as_str)
        }).await
    }

    async fn api_site(ctx: &crate::runtime::RuntimeContext) -> Option<&'static str> {
        site_of(async { assert_refused("api", authorize_api(ctx).await.map(|_| ())); }).await
    }

    /// The protected sources that name sites (the macros' literal arguments).
    const SITE_SOURCES: [&str; 7] = [include_str!("runtime.rs"), include_str!("boot.rs"), include_str!("ingress.rs"),
        include_str!("../runtime/openai_compat.rs"), include_str!("../runtime/codex.rs"),
        include_str!("../runtime/grok.rs"), include_str!("../runtime/generic_cli.rs")];

    /// Every literal passed to a site macro (`site!`, `refuse!`, `keep!`,
    /// boot's `step!`). The needles are built so this function's own text
    /// does not match them.
    fn macro_labels(source: &str) -> Vec<String> {
        let mut labels = Vec::new();
        for name in ["site", "refuse", "keep", "step"] {
            let needle = [name, "!(", "\""].concat();
            for (index, _) in source.match_indices(needle.as_str()) {
                let rest = &source[index + needle.len()..];
                if let Some(end) = rest.find('"') { labels.push(rest[..end].to_owned()); }
            }
        }
        labels
    }

    /// F3: the closed set is structural — a site is a `RefusalSite`, built only
    /// by `site!`, whose compile-time check rejects any label missing from
    /// `refusal::SITES` (so a missing entry is a build failure, not a test
    /// failure). This test keeps the list itself tidy: well-formed, no
    /// duplicates, and no dead entries.
    #[test]
    fn site_labels_are_a_closed_fixed_set() {
        use super::super::refusal::SITES;
        let unique: std::collections::BTreeSet<_> = SITES.iter().collect();
        assert_eq!(unique.len(), SITES.len(), "duplicate site label");
        assert!(!SITES.contains(&"unlabelled"), "`unlabelled` is the logger's fallback text, not a site");
        for site in SITES {
            assert!(LABEL_PREFIXES.iter().any(|prefix| site.starts_with(prefix)), "{site}");
            assert!(site.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_'), "{site}");
        }
        let mut used = std::collections::BTreeSet::new();
        for source in SITE_SOURCES {
            for label in macro_labels(source) {
                assert!(SITES.contains(&label.as_str()), "label missing from refusal::SITES: {label}");
                used.insert(label);
            }
        }
        for site in SITES {
            assert!(used.contains(*site), "unused site label: {site}");
        }
    }

    /// The guard: no refusal on these paths can be built without a site.
    /// Structural since F3: `refuse`/`keep`/`note_site`/`Sited::new` take a
    /// `RefusalSite`, and the boot/ingress protected helpers return
    /// `refusal::Sited` (no `From` conversion), so a new unlabelled exit does
    /// not compile — that compile failure is the negative case. This test
    /// rejects the remaining escape hatches: another refusal builder in
    /// runtime.rs or the API turn, and building a site outside `site!`.
    #[test]
    fn protected_paths_have_no_unlabelled_refusal() {
        let source = include_str!("runtime.rs");
        let production = source.split("#[cfg(test)]\npub(super) mod tests").next().unwrap();
        assert!(production.len() < source.len(), "test-module marker not found");
        for banned in ["refusal()", "ok_or_else(refusal)", "fn refusal(", "spawn_gate_error("] {
            assert!(!production.contains(banned), "unlabelled refusal builder in runtime.rs: {banned}");
        }
        // `refuse` itself and the R8t class message of a started API run.
        assert_eq!(production.matches("user_error(").count(), 2);
        // A site is built only inside `site!`'s const: the crate-wide scan is
        // `refusal::tests::the_site_builder_is_named_only_by_site_macro`.
        // A synthetic negative case for the label scan above.
        let planted = ["refuse", "!(\"plan.planted\")"].concat();
        assert_eq!(macro_labels(&planted), vec!["plan.planted".to_owned()]);
        assert!(!super::super::refusal::SITES.contains(&"plan.planted"));
        let api = include_str!("../runtime/openai_compat.rs");
        let start = api.find("async fn protected_turn").expect("protected API turn");
        let end = start + api[start..].find("pub fn new()").expect("end of the protected API turn");
        let protected = &api[start..end];
        assert!(!protected.contains("refusal()"));
        assert_eq!(protected.matches("spawn_gate_error(").count(), 1, "one labelled builder only");
        assert!(api[..start].contains("isolation_mcp::runtime::observed("), "the API turn has one logging point");
        // F1: the Codex/Grok/registered-CLI protected branches open their
        // observed scope before their own preflight (the entry they call joins
        // it); the Claude callers reach the lookup only through `start_claude`
        // (`claude_max_turns` is private and `spawn_claude` is test-only, so
        // that half is structural).
        for (name, source, preflight) in [
            ("codex", include_str!("../runtime/codex.rs"), "require_mcp_isolation()"),
            ("grok", include_str!("../runtime/grok.rs"), "refuse_unsupported_runtime(\"grok\")"),
            ("generic_cli", include_str!("../runtime/generic_cli.rs"), "preflight(self.spec.id, context)"),
        ] {
            let scope = source.find("isolation_mcp::runtime::observed(").unwrap_or_else(|| panic!("{name}: no scope"));
            let first = source.find(preflight).unwrap_or_else(|| panic!("{name}: no preflight"));
            assert!(scope < first, "{name}: protected preflight runs before the observed scope");
        }
        for caller in [include_str!("../claude_runner.rs"), include_str!("../channel_reply/cli_env.rs")] {
            assert!(caller.contains("isolation_mcp::runtime::start_claude("), "a Claude caller bypasses start_claude");
        }
    }

    #[tokio::test]
    async fn each_authorize_api_condition_yields_its_label() {
        let base = context();
        let mut missing = base.clone();
        missing.agent_dir = None;
        assert_eq!(api_site(&missing).await, Some("authorize.agent_dir_missing"));
        let mut effort = base.clone();
        effort.effort = Some(duduclaw_core::effort::Effort::High);
        assert_eq!(api_site(&effort).await, Some("authorize.effort"));
        assert_eq!(crate::runtime_dispatch::UTILITY_CALL.scope((), api_site(&base)).await,
            Some("authorize.utility_call"));
        assert_eq!(crate::runtime::SPAWN_OVERRIDE.scope(
            crate::runtime::SpawnOverride { work_dir: Some("/tmp".into()) }, api_site(&base)).await,
            Some("authorize.work_dir_override"));
        let mut other = base.clone();
        other.agent_id = "bob".into();
        assert_eq!(api_site(&other).await, Some("authorize.agent_dir"));
        let mut policy = base.clone();
        policy.capabilities = Some(toml::from_str("[[policy]]\ntool = \"shell_exec\"\neffect = \"forbid\"\n").unwrap());
        assert_eq!(api_site(&policy).await, Some("authorize.capabilities_policy"));
        // Past the coarse gates: the authority step that refused.
        assert_eq!(api_site(&base).await, Some("authorization.principal_context"), "no canonical home");
        let (_temp, home) = live_home();
        let live = live_context(&home);
        assert_eq!(api_site(&live).await, Some("authorization.ingress.no_scope"));
        let fake = FakeOrigin::new(None, None);
        assert_eq!(FAKE_ORIGIN.scope(fake, api_site(&live)).await, Some("authorization.ingress_issue"));
        let fake = FakeOrigin::new(Some(None), None);
        assert_eq!(FAKE_ORIGIN.scope(fake, api_site(&live)).await, Some("authorization.background_issue"));
        with_runtime_authorization(authorization_at(&home, "alice", 1), async {
            assert!(site_of(async { authorize_api(&live).await.unwrap(); }).await.is_none());
            assert_eq!(api_site(&live).await, Some("authorization.consumed"));
        }).await;
        with_runtime_authorization(authorization_at(&home, "bob", 1), async {
            assert_eq!(api_site(&live).await, Some("authorization.installed_mismatch"));
        }).await;
    }

    #[test]
    fn spawn_refusals_map_to_fixed_sites_never_text() {
        use duduclaw_isolation::execution::BackendError;
        for (error, site) in [(BackendError::Unavailable, "spawn.backend.unavailable"),
            (BackendError::Denied, "spawn.backend.denied"), (BackendError::WouldDeadlock, "spawn.backend.would_deadlock"),
            (BackendError::NotFound, "spawn.backend.not_found"), (BackendError::Timeout, "spawn.backend.timeout"),
            (BackendError::Io, "spawn.backend.io"), (BackendError::Protocol, "spawn.backend.protocol")] {
            assert_eq!(spawn_site(&std::io::Error::new(std::io::ErrorKind::Other, error)).as_str(), site);
        }
        for (kind, site) in [(std::io::ErrorKind::PermissionDenied, "spawn.start.permission_denied"),
            (std::io::ErrorKind::NotFound, "spawn.start.not_found"), (std::io::ErrorKind::TimedOut, "spawn.start.timed_out"),
            (std::io::ErrorKind::BrokenPipe, "spawn.start.other")] {
            assert_eq!(spawn_site(&std::io::Error::new(kind, "planted spawnd text")).as_str(), site);
        }
    }

    fn capture_warnings() -> (LogCapture, tracing::subscriber::DefaultGuard) {
        let log = LogCapture(Default::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
            .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
        (log, tracing::subscriber::set_default(subscriber))
    }

    /// R8x F1, at the Claude caller boundary: `start_claude` is exactly what
    /// both production callers run (lookup, then spawn). A refused protected
    /// step-limit lookup (no booted root publication here) is the turn's one
    /// `isolated run not started` line with its `step_limit.*` site — no
    /// separate site-less warning — before any authority or prompt work.
    #[tokio::test]
    async fn a_refused_step_limit_lookup_is_the_turns_one_line() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let (log, _default) = capture_warnings();
        let prompt_built = AtomicBool::new(false);
        with_runtime_authorization(authorization("alice"), async {
            let error = start_claude(Path::new("/trusted-home/agents/alice"), 50, "input",
                |_| { prompt_built.store(true, SeqCst); String::new() }, "model").await.err().expect("refused");
            // The user-visible refusal is unchanged.
            assert_eq!(error, super::super::refusal::user_error(None));
            // Refused before authority: the installed authorization is untouched.
            assert_eq!(take_authorization(Path::new("/trusted-home/agents/alice")).unwrap().agent, "alice");
        }).await;
        assert!(!prompt_built.load(SeqCst), "no prompt is built for a refused lookup");
        assert!(start_claude(Path::new("/"), 50, "input", |_| String::new(), "model").await.is_err());
        // The callers' own option gate, before the start: same text, one line.
        assert_eq!(refuse_claude_options(Some(Path::new("/trusted-home/agents/alice")), "options text".into()).await,
            "options text");
        let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = logs.lines().collect();
        assert_eq!(lines.len(), 3, "{logs}");
        for field in ["isolated run not started", "runtime=\"claude\"", "agent=\"alice\"", "site=\"step_limit.unavailable\""] {
            assert!(lines[0].contains(field), "{field}: {logs}");
        }
        assert!(lines[1].contains("isolated run not started") && lines[1].contains("site=\"step_limit.agent_dir\"")
            && lines[1].contains("agent=\"invalid\""), "{logs}");
        assert!(lines[2].contains("agent=\"alice\"") && lines[2].contains("site=\"preflight.claude_options\""), "{logs}");
        assert!(!logs.contains("protected step limit") && !logs.contains("options text"), "{logs}");
    }

    /// R8x F1: an inner observed scope (an entry point called from a runtime's
    /// protected branch) joins the outer turn: one line, the outer runtime
    /// label, the first site recorded in either scope, and `started` kept.
    #[tokio::test]
    async fn an_inner_observed_scope_joins_the_outer_turn() {
        let (log, _default) = capture_warnings();
        observed("codex", "alice", async {
            observed("codex", "alice", async { Err::<(), _>(refuse!("authorize.native_gate")) }).await
        }).await.unwrap_err();
        observed("grok", "alice", async {
            let _ = keep(site!("preflight.explore_lane"))(String::new());
            observed("grok", "alice", async { Err::<(), _>(refuse!("run.input")) }).await
        }).await.unwrap_err();
        observed("codex", "alice", async {
            observed("codex", "alice", async {
                record(|trace| trace.started = true);
                Ok::<_, String>(())
            }).await?;
            Err::<(), _>(keep(site!("output.empty"))(String::from("isolated Codex returned no answer")))
        }).await.unwrap_err();
        observed("codex", "alice", async {
            observed("codex", "alice", async { Ok::<_, String>(()) }).await
        }).await.unwrap();
        let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = logs.lines().collect();
        assert_eq!(lines.len(), 3, "{logs}");
        assert!(lines[0].contains("isolated run not started") && lines[0].contains("runtime=\"codex\"")
            && lines[0].contains("site=\"authorize.native_gate\""), "{logs}");
        assert!(lines[1].contains("runtime=\"grok\"") && lines[1].contains("site=\"preflight.explore_lane\""), "{logs}");
        assert!(lines[2].contains("isolated run refused after start") && lines[2].contains("site=\"output.empty\""),
            "{logs}");
    }

    #[derive(Clone)]
    struct LogCapture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    /// One failed turn ⇒ exactly one line with fixed fields: `not started`
    /// (with the site, or `unlabelled`), `refused after start`, or nothing
    /// extra when the R8t line was written. Success logs nothing; request
    /// content and an invalid agent label never appear.
    #[tokio::test]
    async fn a_refused_turn_logs_exactly_one_fixed_line() {
        const MARKER: &str = "R8X-PLANTED-MARKER-41c9";
        let log = LogCapture(Default::default());
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false)
            .with_max_level(tracing::Level::WARN).with_writer(move || writer.clone()).finish();
        let _default = tracing::subscriber::set_default(subscriber);
        let mut ctx = context();
        ctx.effort = Some(duduclaw_core::effort::Effort::High);
        ctx.system_prompt = MARKER.into();
        ctx.model = MARKER.into();
        let error = observed("openai_compat", &ctx.agent_id, async { authorize_api(&ctx).await.map(|_| ()) })
            .await.unwrap_err();
        // The user-visible refusal is exactly the pre-R8x sentence.
        assert_eq!(error, super::super::refusal::user_error(None));
        observed("openai_compat", "alice", async { Err::<(), _>(format!("{MARKER} provider text")) }).await.unwrap_err();
        observed("openai_compat", "alice", async {
            record(|trace| trace.started = true);
            Err::<(), _>(refuse!("output.parse"))
        }).await.unwrap_err();
        observed("openai_compat", "alice", async {
            record(|trace| trace.started = true);
            record(|trace| trace.logged = true);
            Err::<(), _>(refuse!("run.refused"))
        }).await.unwrap_err();
        observed("openai_compat", "alice", async { Ok::<_, String>(()) }).await.unwrap();
        observed("openai_compat", "../etc/passwd", async { Err::<(), _>(refuse!("request.encode")) }).await.unwrap_err();
        let logs = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = logs.lines().collect();
        assert_eq!(lines.len(), 4, "{logs}");
        for field in ["isolated run not started", "runtime=\"openai_compat\"", "agent=\"alice\"", "site=\"authorize.effort\""] {
            assert!(lines[0].contains(field), "{field}: {logs}");
        }
        assert!(lines[1].contains("isolated run not started") && lines[1].contains("site=\"unlabelled\""), "{logs}");
        assert!(lines[2].contains("isolated run refused after start") && lines[2].contains("site=\"output.parse\""), "{logs}");
        assert!(lines[3].contains("agent=\"invalid\"") && lines[3].contains("site=\"request.encode\""), "{logs}");
        assert!(!logs.contains(MARKER) && !logs.contains("passwd") && !logs.contains("provider text"), "{logs}");
        assert_eq!(logs.matches("isolated run not started").count(), 3, "{logs}");
    }
}
