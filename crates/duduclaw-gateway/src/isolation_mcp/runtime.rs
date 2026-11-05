//! One-shot host authorization and the Claude main-run execution context.
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

/// The writer drains concurrently with runtime output. Its error must be
/// observed before accepting a successful result; drop cancels pending input.
pub(crate) struct PromptWriter(tokio::task::JoinHandle<Result<(), String>>);
impl PromptWriter {
    pub(crate) async fn finish(&mut self) -> Result<(), String> {
        (&mut self.0).await.map_err(|_| refusal())?
    }
}
impl Drop for PromptWriter { fn drop(&mut self) { self.0.abort(); } }

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
    let authorization = take_authorization(agent_dir)?;
    let class = crate::process_group::execution_class(ExecutionClass::EmployeeMain);
    let plan = super::boot::claude_plan(authorization.home, authorization.agent.clone(), class,
        prompt.as_bytes().to_vec(), system_prompt.as_bytes().to_vec(), model.into()).await.map_err(|_| refusal())?;
    let launch = super::prepare_launch(super::LaunchInput { agent: authorization.agent, plan,
        grant: authorization.grant, roots: authorization.roots, origin: authorization.origin,
        caller: authorization.caller, turn: authorization.turn, authenticated_epoch: authorization.authenticated_epoch })
        .await.map_err(|_| refusal())?;
    // This builder is only an adapter marker. A cannot execute it locally: the
    // protected latch is required and the root publication selects the binary.
    if !duduclaw_core::execution_gate::isolation_required() { return Err(refusal()); }
    let mut marker = tokio::process::Command::new("root-selected-claude");
    let admission = crate::autonomous_runtime::admit_current().await?;
    let mut child = duduclaw_execution::command::with_context(launch,
        duduclaw_execution::command::with_runtime(RuntimeKind::Claude,
            crate::process_group::BackendChild::spawn_class(&mut marker, class))).await
        .map_err(|_| refusal())?;
    crate::autonomous_runtime::mark_handoff();
    drop(admission);
    let stdin = child.child().stdin.take().ok_or_else(refusal)?;
    let bytes = prompt.as_bytes().to_vec();
    let control = child.child().control();
    let writer = tokio::spawn(async move {
        let result = write_prompt(stdin, &bytes, Duration::from_secs(30)).await;
        if result.is_err() {
            let _ = control.start_kill();
            return Err(refusal());
        }
        Ok(())
    });
    Ok((child, PromptWriter(writer)))
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
