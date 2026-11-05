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

pub(crate) async fn spawn_claude(agent_dir: &std::path::Path, prompt: &str, system_prompt: &str, model: &str)
    -> Result<(crate::process_group::BackendChild, PromptWriter), String> {
    let (authorization, principal_context) = if AUTHORIZATION.try_with(|_| ()).is_ok() {
        (take_authorization(agent_dir)?, None)
    } else {
        let agent = agent_dir.file_name().and_then(|name| name.to_str()).ok_or_else(refusal)?;
        let home = agent_dir.parent().and_then(|path| path.parent()).ok_or_else(refusal)?;
        let guard = crate::on_behalf_of::context::register(home, agent).ok_or_else(refusal)?;
        let authorization = if crate::isolation_background::has_scope() {
            crate::isolation_background::authorize_current(agent_dir, Some(guard.reference().into())).await
        } else {
            super::ingress::authorize_current(agent_dir, Some(guard.reference().into())).await
        }.map_err(|_| refusal())?;
        (authorization, Some(guard))
    };
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::claude_plan(authorization.home.clone(), authorization.agent.clone(), class,
        prompt.as_bytes().to_vec(), system_prompt.as_bytes().to_vec(), model.into()).await.map_err(|_| refusal())?;
    spawn_plan(authorization, plan, class, RuntimeKind::Claude, prompt.as_bytes().to_vec()).await
}

async fn spawn_plan(authorization: RuntimeAuthorization, plan: duduclaw_isolation::execution::SpawnPolicy,
    class: ExecutionClass, runtime: RuntimeKind, bytes: Vec<u8>)
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
    let agent_dir = context.agent_dir.as_deref().ok_or_else(refusal)?;
    if context.effort.is_some() || crate::runtime_dispatch::in_utility_call()
        || crate::runtime::spawn_work_dir_override().is_some()
        || agent_dir != context.home_dir.join("agents").join(&context.agent_id) {
        return Err(refusal());
    }
    let runtime = RuntimeKind::Registered(Text::new(format!("generic-cli-v1.{id}")).map_err(|_| refusal())?);
    duduclaw_mcp_boundary::config::registered_cli_arguments(&runtime).map_err(|_| refusal())?;
    let authorization = take_authorization(agent_dir)?;
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
    let (mut child, mut writer) = spawn_plan(authorization, plan, class, runtime, bytes).await?;
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
    let authorization = take_authorization(agent_dir)?;
    let name = if runtime == RuntimeKind::Codex { "codex" } else { "grok" };
    crate::product_policy::require_runtime(&context.home_dir, name).map_err(|_| refusal())?;
    crate::product_policy::require_model(crate::product_policy::load(&context.home_dir).map_err(|_| refusal())?,
        &context.model).map_err(|_| refusal())?;
    if crate::ccr_runtime::for_agent(&context.home_dir, &context.agent_id).is_some()
        || crate::redaction_proxy::try_build_interceptor(&context.home_dir, &context.agent_id,
            &crate::redaction_proxy::current_session_id()).map_err(|_| refusal())?.is_some() { return Err(refusal()); }
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

/// Consume initiating authority before resolving provider credentials. Unsupported
/// utility, workspace and effort modes never enter the worker's provider path.
pub(crate) fn authorize_api(context: &crate::runtime::RuntimeContext) -> Result<RuntimeAuthorization, String> {
    let agent_dir = context.agent_dir.as_deref().ok_or_else(refusal)?;
    if context.effort.is_some() || crate::runtime_dispatch::in_utility_call()
        || crate::runtime::spawn_work_dir_override().is_some()
        || agent_dir != context.home_dir.join("agents").join(&context.agent_id)
        || context.capabilities.as_ref().is_some_and(|caps| !caps.policy.is_empty()) {
        return Err(refusal());
    }
    take_authorization(agent_dir)
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
        RuntimeAuthorization { home: "/trusted-home".into(), agent: agent.into(), grant: GrantPolicy {
            task: None, principal: Principal::Background { tenant_id: Text::new("tenant".into()).unwrap(), generation: 1 },
            binding: EmployeeBinding::Shared(Text::new("tenant".into()).unwrap()), lane: Lane::Standard,
            tools: List::new(vec![]).unwrap(), resources: List::new(vec![]).unwrap(), policy_revision: 1,
            not_before: 1, expires_at: 2 }, roots: Arc::new(ResourceRoots::default()),
            origin: WaitOrigin::Gateway, caller: None, turn: None, authenticated_epoch: None }
    }
    fn context() -> crate::runtime::RuntimeContext {
        crate::runtime::RuntimeContext { agent_dir: Some("/trusted-home/agents/alice".into()),
            system_prompt: "system".into(), model: "model".into(), max_tokens: 100,
            home_dir: "/trusted-home".into(), agent_id: "alice".into(), preferred_provider: None,
            conversation_history: vec![], capabilities: None, account_pool: vec![], effort: None,
            allow_cross_family_failover: true }
    }
    #[tokio::test]
    async fn other_runtimes_refuse_missing_authority_before_boot_credentials_or_spawn() {
        let context = context();
        assert!(authorize_api(&context).is_err());
        let error = execute_registered_cli("qwen", "prompt", &context,
            duduclaw_core::runtime_catalog::OutputFormat::Text).await.unwrap_err();
        assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
    }
    #[tokio::test]
    async fn native_runtimes_refuse_missing_authority_before_boot_config_or_spawn() {
        for kind in [RuntimeKind::Codex, RuntimeKind::Grok] {
            let error = execute_native(kind,"prompt".into(),&context()).await.unwrap_err();
            assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
        }
    }
    #[tokio::test]
    async fn api_authority_is_one_shot_and_rejects_context_identity_mismatch() {
        with_runtime_authorization(authorization("alice"), async {
            let mut wrong = context(); wrong.agent_id = "bob".into();
            assert!(authorize_api(&wrong).is_err());
            let valid = context();
            assert!(authorize_api(&valid).is_ok());
            assert!(authorize_api(&valid).is_err());
        }).await;
    }
    #[tokio::test]
    async fn missing_runtime_authorization_refuses_before_boot_or_spawn() {
        assert!(spawn_claude(std::path::Path::new("/not-an-agent"), "input", "system", "model").await.is_err());
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
