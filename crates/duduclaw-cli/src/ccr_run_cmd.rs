//! Controlled, local four-arm execution over recorded task and source bytes.
//!
//! A file supplied to this command is not an upstream ACL or billing receipt.
//! The only callable external source is the read-only in-memory fixture below;
//! CCR handles are scoped but unbound to any independently verified artifact.

use std::{collections::BTreeSet, fs, io::Write, path::Path, sync::Mutex};

use async_trait::async_trait;
use duduclaw_core::error::Result;
use duduclaw_gateway::{
    ccr_replay::{
        Arm, MAX_REPLAY_INPUT_BYTES, ReplayArm, ReplayAuthorization, ReplayEvidence, ReplayOrigin,
        ReplayTask, error, evaluate_replay_bytes,
    },
    ccr_runtime::source_acl_for_principal,
};
use duduclaw_llm::{
    CCR_FIND_TOOL, CCR_RETRIEVE_TOOL, CcrRuntime, CcrScope, CcrStore, ChatMessage, ChatProvider,
    ChatRequest, ChatResponse, LlmError, ProvenanceConfig, StreamEvent, SystemBlock, ToolChoice,
    ToolDef, ToolExecutor, ToolOutcome, providers::build_provider, resolve_env_key,
    run_tool_loop_with_provenance_and_ccr,
};
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_TASKS: usize = 32;
const MAX_ITERATIONS: usize = 8;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunInput {
    version: String,
    origin: ReplayOrigin,
    authorization: ReplayAuthorization,
    settings: RunSettings,
    tasks: Vec<RunTask>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RunSettings {
    #[serde(default)]
    system_prompt: String,
    max_tokens: u32,
    #[serde(default)]
    temperature: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunTask {
    task_id: String,
    task_input: String,
    source_result: String,
    oracle_exact: String,
}

fn valid_label(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= max
        && !value.chars().any(char::is_control)
}

fn valid_tool_name(value: &str) -> bool {
    valid_label(value, 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn validate(input: &RunInput, provider: &dyn ChatProvider) -> Result<()> {
    let auth = &input.authorization;
    if input.version != "ccr-controlled-run-v1"
        || input.tasks.is_empty()
        || input.tasks.len() > MAX_TASKS
        || !valid_label(&auth.tenant_id, 128)
        || !valid_label(&auth.agent_id, 512)
        || !valid_label(&auth.session_id, 512)
        || !valid_label(&auth.principal_id, 512)
        || !valid_label(&auth.source_acl, 512)
        || !valid_label(&auth.source_server, 512)
        || !valid_tool_name(&auth.source_tool)
        || !valid_label(&auth.provider_id, 512)
        || !valid_label(&auth.model_id, 512)
        || auth.source_tool == CCR_FIND_TOOL
        || auth.source_tool == CCR_RETRIEVE_TOOL
        || !(auth.provider_id == provider.id()
            || (auth.provider_id == "google" && provider.id() == "gemini"))
        || source_acl_for_principal(&auth.agent_id, &auth.session_id, &auth.principal_id).as_deref()
            != Some(auth.source_acl.as_str())
        || input.settings.system_prompt.len() > 8_192
        || !(1..=1_024).contains(&input.settings.max_tokens)
        || input.settings.temperature.is_some_and(|temperature| {
            !temperature.is_finite() || !(0.0..=2.0).contains(&temperature)
        })
    {
        return Err(error("invalid controlled run settings or scope"));
    }
    let mut task_ids = BTreeSet::new();
    let mut source_bytes = 0_usize;
    let mut prompt_bytes = 0_usize;
    for task in &input.tasks {
        source_bytes = source_bytes.saturating_add(task.source_result.len());
        prompt_bytes = prompt_bytes.saturating_add(task.task_input.len());
        if !valid_label(&task.task_id, 128)
            || !task_ids.insert(&task.task_id)
            || task.task_input.is_empty()
            || task.task_input.len() > 65_536
            || task.source_result.is_empty()
            || task.source_result.len() > 2 * 1024 * 1024
            || !(3..=1_024).contains(&task.oracle_exact.len())
            || task.oracle_exact.chars().any(char::is_control)
            || !task.source_result.contains(&task.oracle_exact)
            || source_bytes > 1_000_000
            || prompt_bytes > 128_000
        {
            // Caller text (including task IDs) never enters an error.
            return Err(error("invalid controlled run task"));
        }
    }
    Ok(())
}

struct SourceOnlyExecutor {
    server: String,
    tool: String,
    delivered: String,
}

#[async_trait]
impl ToolExecutor for SourceOnlyExecutor {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: self.tool.clone(),
            description: "Read the recorded task source for this evaluation".into(),
            input_schema: json!({"type":"object","properties":{},"additionalProperties":false}),
        }]
    }

    fn server_of(&self, tool: &str) -> Option<String> {
        (tool == self.tool).then(|| self.server.clone())
    }

    async fn call(&self, name: &str, args: Value) -> std::result::Result<ToolOutcome, String> {
        if name != self.tool || !args.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err("recorded source route requires its exact empty arguments".into());
        }
        // This file-backed result must remain unbound: no `source_artifact`
        // claim and no fake independent source verifier.
        Ok(ToolOutcome::ok(self.delivered.clone()))
    }
}

/// Captures exactly the provider responses the native loop consumes. The
/// source route is forced only for round one; later rounds can finish or use
/// CCR find/retrieve. The loop's initial request itself remains arm-invariant.
struct RecordingProvider<'a> {
    inner: &'a dyn ChatProvider,
    source_tool: &'a str,
    responses: Mutex<Vec<ChatResponse>>,
}

impl RecordingProvider<'_> {
    fn recorded(&self) -> Vec<ChatResponse> {
        self.responses.lock().expect("recording lock").clone()
    }
}

#[async_trait]
impl ChatProvider for RecordingProvider<'_> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn complete(&self, req: &ChatRequest) -> std::result::Result<ChatResponse, LlmError> {
        let first = self.responses.lock().expect("recording lock").is_empty();
        let mut controlled = req.clone();
        controlled.tool_choice = if first {
            ToolChoice::Tool(self.source_tool.to_owned())
        } else {
            ToolChoice::Auto
        };
        let response = self.inner.complete(&controlled).await?;
        self.responses
            .lock()
            .expect("recording lock")
            .push(response.clone());
        Ok(response)
    }

    async fn stream(
        &self,
        _req: &ChatRequest,
    ) -> std::result::Result<BoxStream<'static, std::result::Result<StreamEvent, LlmError>>, LlmError>
    {
        Err(LlmError::InvalidRequest(
            "controlled CCR run requires nonstreaming completion".into(),
        ))
    }
}

fn settings_json(settings: &RunSettings, auth: &ReplayAuthorization) -> Result<String> {
    let system_sha256 = hex::encode(Sha256::digest(settings.system_prompt.as_bytes()));
    serde_json::to_string(&json!({
        "system_prompt_sha256": system_sha256,
        "requested_provider_id": auth.provider_id,
        "requested_model_id": auth.model_id,
        "max_tokens": settings.max_tokens,
        "temperature": settings.temperature,
        "first_round_tool_choice": auth.source_tool,
        "subsequent_tool_choice": "auto",
        "source_schema": "empty_object_v1",
        "max_tool_iterations": MAX_ITERATIONS,
    }))
    .map_err(error)
}

async fn run_with_provider(
    input: RunInput,
    provider: &dyn ChatProvider,
) -> Result<(Value, ReplayEvidence)> {
    validate(&input, provider)?;
    let requested_auth = input.authorization;
    let model_settings_json = settings_json(&input.settings, &requested_auth)?;
    let mut auth = requested_auth.clone();
    auth.provider_id = provider.id().to_owned();
    let mut observed_model_id: Option<String> = None;
    let mut evidence_tasks = Vec::with_capacity(input.tasks.len());
    for task in input.tasks {
        let mut arms = Vec::with_capacity(4);
        for arm in Arm::ALL {
            // Isolation prevents one arm from finding another arm's CCR
            // handles while preserving the same replay authorization digest.
            let dir = tempfile::tempdir().map_err(error)?;
            let scope = CcrScope {
                tenant_id: auth.tenant_id.clone(),
                agent_id: auth.agent_id.clone(),
                session_id: auth.session_id.clone(),
                source_acl: auth.source_acl.clone(),
            };
            let runtime = CcrRuntime::new(CcrStore::new(dir.path().join("ccr.db")), scope)
                .restrict_sources([(auth.source_server.clone(), auth.source_tool.clone())]);
            let delivered =
                super::ccr_compare_cmd::arm_delivery(arm, &task.source_result, None, &runtime);
            let tools = SourceOnlyExecutor {
                server: auth.source_server.clone(),
                tool: auth.source_tool.clone(),
                delivered,
            };
            let mut request = ChatRequest::new(format!(
                "{}/{}",
                requested_auth.provider_id, requested_auth.model_id
            ));
            if !input.settings.system_prompt.is_empty() {
                request
                    .system
                    .push(SystemBlock::uncached(&input.settings.system_prompt));
            }
            request
                .messages
                .push(ChatMessage::user(task.task_input.clone()));
            request.max_tokens = input.settings.max_tokens;
            request.temperature = input.settings.temperature;
            let recording = RecordingProvider {
                inner: provider,
                source_tool: &auth.source_tool,
                responses: Mutex::new(Vec::new()),
            };
            let outcome = run_tool_loop_with_provenance_and_ccr(
                &recording,
                request,
                &tools,
                MAX_ITERATIONS,
                ProvenanceConfig::default(),
                None,
                (arm == Arm::Ccr).then_some(runtime),
            )
            .await
            // Provider errors can contain request excerpts. Keep CLI errors
            // content-free even when the adapter includes upstream bodies.
            .map_err(|_| error("provider or native tool loop failed"))?;
            let responses = recording.recorded();
            if responses.is_empty()
                || responses.iter().any(|response| {
                    response.provider != provider.id()
                        || !valid_label(&response.model_used, 512)
                        || observed_model_id
                            .as_deref()
                            .is_some_and(|model| model != response.model_used)
                })
            {
                return Err(error("provider/model changed during controlled run"));
            }
            let arm_model_id = &responses[0].model_used;
            if responses
                .iter()
                .any(|response| response.model_used != arm_model_id.as_str())
            {
                return Err(error("provider/model changed during controlled run"));
            }
            if observed_model_id.is_none() {
                observed_model_id = Some(arm_model_id.clone());
            }
            let mut arm_auth = auth.clone();
            arm_auth.model_id = observed_model_id
                .clone()
                .expect("observed response checked");
            arms.push(ReplayArm {
                arm,
                task_input: task.task_input.clone(),
                source_result: task.source_result.clone(),
                delivered_source_sha256: Some(hex::encode(Sha256::digest(
                    tools.delivered.as_bytes(),
                ))),
                model_settings_json: model_settings_json.clone(),
                authorization: arm_auth,
                responses,
                telemetry: outcome.telemetry,
            });
        }
        evidence_tasks.push(ReplayTask {
            task_id: task.task_id,
            oracle_exact: task.oracle_exact,
            arms,
        });
    }
    let evidence = ReplayEvidence {
        version: "ccr-native-replay-v1".into(),
        origin: input.origin,
        tasks: evidence_tasks,
    };
    let bytes = serde_json::to_vec(&evidence).map_err(error)?;
    let mut report = evaluate_replay_bytes(&bytes, Some(&auth.tenant_id))?;
    report["execution_method"] = json!("controlled_local_provider_run");
    report["source_attestation"] = json!("recorded_file_claim_unverified");
    report["billing_status"] = json!("unavailable_without_verified_receipts");
    Ok((report, evidence))
}

fn read_input(path: &Path) -> Result<RunInput> {
    if fs::metadata(path).map_err(error)?.len() > MAX_REPLAY_INPUT_BYTES as u64 {
        return Err(error("controlled run input exceeds 16 MB"));
    }
    let bytes = fs::read(path).map_err(error)?;
    if bytes.len() > MAX_REPLAY_INPUT_BYTES {
        return Err(error("controlled run input exceeds 16 MB"));
    }
    serde_json::from_slice(&bytes).map_err(|_| error("invalid controlled run JSON"))
}

fn write_private_evidence(path: &Path, evidence: &ReplayEvidence) -> Result<()> {
    let bytes = serde_json::to_vec(evidence).map_err(error)?;
    if bytes.len() > MAX_REPLAY_INPUT_BYTES {
        return Err(error("raw evidence exceeds 16 MB"));
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    {
        return Err(error(
            "private evidence output requires Unix file permissions",
        ));
    }
    let mut file = options.open(path).map_err(error)?;
    if let Err(write_error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error(format!(
            "private evidence write failed: {write_error}"
        )));
    }
    Ok(())
}

/// Whether `--base-url` may receive the provider's real API key as a bearer
/// token.
///
/// Parsed, never prefix-matched (coding convention 2): `http://localhost:1@evil.example/`
/// passes a `starts_with("http://localhost:")` test while actually connecting
/// to `evil.example`. Userinfo is refused outright — a URL that carries
/// credentials of its own has no business in a bearer-token flow — and plain
/// HTTP is confined to the loopback hosts.
fn provider_base_url_allowed(base: &str) -> bool {
    let Ok(parsed) = url::Url::parse(base) else {
        return false;
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    match parsed.scheme() {
        "https" => true,
        // `host_str()` keeps the brackets on an IPv6 literal, so match the
        // parsed host instead of its spelling.
        "http" => match parsed.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            None => false,
        },
        _ => false,
    }
}

pub async fn run(path: &Path, evidence_out: Option<&Path>, base_url: Option<&str>) -> Result<()> {
    let input = read_input(path)?;
    let provider_id = &input.authorization.provider_id;
    let api_key = resolve_env_key(provider_id)
        .ok_or_else(|| error("configured provider credential is unavailable"))?;
    let mut auth = duduclaw_llm::ApiAuth::new(api_key);
    if let Some(base) = base_url {
        if !provider_base_url_allowed(base) {
            return Err(error("provider base URL must use HTTPS or loopback HTTP"));
        }
        auth = auth.with_base_url(base);
    }
    let provider =
        build_provider(provider_id, auth).ok_or_else(|| error("unsupported provider adapter"))?;
    let (report, evidence) = run_with_provider(input, provider.as_ref()).await?;
    if let Some(path) = evidence_out {
        write_private_evidence(path, &evidence)?;
    }
    println!("{}", serde_json::to_string_pretty(&report).map_err(error)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use duduclaw_llm::{ContentPart, NormalizedUsage, Role, StopReason};

    use super::*;

    #[test]
    fn userinfo_cannot_smuggle_a_remote_host_past_the_loopback_check() {
        // The bug: a prefix test on "http://localhost:" accepted this, then the
        // real provider API key went to `evil.example` as a bearer token.
        for smuggled in [
            "http://localhost:1@evil.example/",
            "http://127.0.0.1:1@evil.example/v1",
            "http://user@localhost:8080/",
            "https://user:pass@api.example.com/",
            "http://evil.example/",
            "http://localhost.evil.example/",
            "ftp://127.0.0.1/",
            "not a url",
        ] {
            assert!(
                !provider_base_url_allowed(smuggled),
                "{smuggled} must be refused"
            );
        }
        for allowed in [
            "https://api.example.com/v1",
            "http://127.0.0.1:8080/v1",
            "http://localhost:1234",
            "http://[::1]:8080/v1",
        ] {
            assert!(
                provider_base_url_allowed(allowed),
                "{allowed} must be allowed"
            );
        }
    }

    struct MockProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ChatProvider for MockProvider {
        fn id(&self) -> &str {
            "synthetic"
        }

        async fn complete(&self, req: &ChatRequest) -> std::result::Result<ChatResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let last_tool = req.messages.iter().rev().find_map(|message| {
                (message.role == Role::Assistant)
                    .then(|| {
                        message.parts.iter().find_map(|part| match part {
                            ContentPart::ToolCall { name, .. } => Some(name.as_str()),
                            _ => None,
                        })
                    })
                    .flatten()
            });
            let last_result = req
                .messages
                .last()
                .and_then(|message| {
                    message.parts.iter().find_map(|part| match part {
                        ContentPart::ToolResult { content, .. } => Some(content.as_str()),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            let (parts, stop) = match last_tool {
                None => {
                    assert_eq!(req.tool_choice, ToolChoice::Tool("fixture_read".into()));
                    (
                        vec![ContentPart::ToolCall {
                            id: "source".into(),
                            name: "fixture_read".into(),
                            args: json!({}),
                        }],
                        StopReason::ToolUse,
                    )
                }
                Some("fixture_read") if last_result.contains("id=") => {
                    assert_eq!(req.tool_choice, ToolChoice::Auto);
                    (
                        vec![ContentPart::ToolCall {
                            id: "find".into(),
                            name: CCR_FIND_TOOL.into(),
                            args: json!({"query":"UNIQUE-MIDDLE-001"}),
                        }],
                        StopReason::ToolUse,
                    )
                }
                Some(CCR_FIND_TOOL) => {
                    let hit: Value = serde_json::from_str(last_result).unwrap();
                    let id = hit["hits"][0]["id"].as_str().unwrap();
                    (
                        vec![ContentPart::ToolCall {
                            id: "retrieve".into(),
                            name: CCR_RETRIEVE_TOOL.into(),
                            args: json!({"id":id,"query":"UNIQUE-MIDDLE-001"}),
                        }],
                        StopReason::ToolUse,
                    )
                }
                _ if last_result.contains("UNIQUE-MIDDLE-001") => (
                    vec![ContentPart::Text("UNIQUE-MIDDLE-001".into())],
                    StopReason::EndTurn,
                ),
                _ => (
                    vec![ContentPart::Text("MISSING".into())],
                    StopReason::EndTurn,
                ),
            };
            Ok(ChatResponse {
                parts,
                stop,
                usage: NormalizedUsage {
                    input_tokens: 5,
                    output_tokens: 2,
                    ..Default::default()
                },
                model_used: "mock-v1".into(),
                provider: "synthetic".into(),
            })
        }

        async fn stream(
            &self,
            _req: &ChatRequest,
        ) -> std::result::Result<
            BoxStream<'static, std::result::Result<StreamEvent, LlmError>>,
            LlmError,
        > {
            Err(LlmError::InvalidRequest("unused".into()))
        }
    }

    fn input() -> RunInput {
        let source_acl = source_acl_for_principal("agent", "session", "principal").unwrap();
        RunInput {
            version: "ccr-controlled-run-v1".into(),
            origin: ReplayOrigin::Synthetic,
            authorization: ReplayAuthorization {
                tenant_id: "tenant".into(),
                agent_id: "agent".into(),
                session_id: "session".into(),
                principal_id: "principal".into(),
                source_acl,
                source_server: "recorded".into(),
                source_tool: "fixture_read".into(),
                provider_id: "synthetic".into(),
                model_id: "mock-latest".into(),
            },
            settings: RunSettings {
                system_prompt: "Use the source tool".into(),
                max_tokens: 512,
                temperature: Some(0.0),
            },
            tasks: vec![RunTask {
                task_id: "Private ticket subject".into(),
                task_input: "Find the exact private ticket value".into(),
                source_result: format!(
                    "{}\nUNIQUE-MIDDLE-001\n{}",
                    "routine line\n".repeat(200),
                    "routine ending\n".repeat(200)
                ),
                oracle_exact: "UNIQUE-MIDDLE-001".into(),
            }],
        }
    }

    #[tokio::test]
    async fn controlled_run_executes_four_native_arms_and_replays_exact_usage() {
        let provider = MockProvider {
            calls: AtomicUsize::new(0),
        };
        let (report, evidence) = run_with_provider(input(), &provider).await.unwrap();
        assert_eq!(evidence.tasks[0].arms.len(), 4);
        assert!(evidence.tasks[0].arms.iter().all(|arm| {
            arm.authorization.model_id == "mock-v1"
                && arm.model_settings_json.contains("mock-latest")
        }));
        assert!(
            evidence.tasks[0]
                .arms
                .iter()
                .all(|arm| arm.source_result == evidence.tasks[0].arms[0].source_result)
        );
        for arm in &evidence.tasks[0].arms {
            assert_eq!(arm.telemetry.provider_rounds, arm.responses.len() as u64);
            assert_eq!(
                arm.telemetry.provider_usage.input_tokens,
                5 * arm.responses.len() as u64
            );
            assert_eq!(
                arm.telemetry.provider_usage.output_tokens,
                2 * arm.responses.len() as u64
            );
        }
        let ccr = &evidence.tasks[0].arms[3];
        assert!(ccr.telemetry.ccr_compressed_results > 0);
        assert_eq!(ccr.telemetry.ccr_find_attempts, 1);
        assert_eq!(ccr.telemetry.ccr_retrieve_attempts, 1);
        assert_eq!(report["report"]["paired_tasks"], 1);
        assert_eq!(report["source_delivery"][0]["changed_tasks"], 0);
        assert_eq!(report["source_delivery"][3]["changed_tasks"], 1);
        let arms = report["report"]["arms"].as_array().unwrap();
        assert_eq!(arms[0]["successes"], 1);
        assert_eq!(arms[1]["successes"], 1);
        assert_eq!(arms[2]["successes"], 0);
        assert_eq!(arms[3]["successes"], 1);
        assert_eq!(
            report["billing_status"],
            "unavailable_without_verified_receipts"
        );
        let rendered = report.to_string();
        assert!(!rendered.contains("Private ticket subject"));
        assert!(!rendered.contains("Find the exact private ticket value"));
        assert!(!rendered.contains("UNIQUE-MIDDLE-001"));
        assert!(!rendered.contains("routine line"));
    }

    #[tokio::test]
    async fn short_source_discloses_noop_treatments() {
        let provider = MockProvider {
            calls: AtomicUsize::new(0),
        };
        let mut task = input();
        task.tasks[0].source_result = "Ticket UNIQUE-MIDDLE-001".into();
        let (report, _) = run_with_provider(task, &provider).await.unwrap();
        let delivery = report["source_delivery"].as_array().unwrap();
        assert_eq!(delivery.len(), 4);
        assert!(delivery.iter().all(|arm| arm["changed_tasks"] == 0));
    }

    #[tokio::test]
    async fn controlled_run_rejects_scope_route_and_provider_before_calls() {
        let provider = MockProvider {
            calls: AtomicUsize::new(0),
        };
        let mut wrong_scope = input();
        wrong_scope.authorization.source_acl = "other".into();
        assert!(run_with_provider(wrong_scope, &provider).await.is_err());
        let mut wrong_route = input();
        wrong_route.authorization.source_tool = CCR_FIND_TOOL.into();
        assert!(run_with_provider(wrong_route, &provider).await.is_err());
        let mut wrong_provider = input();
        wrong_provider.authorization.provider_id = "openai".into();
        assert!(run_with_provider(wrong_provider, &provider).await.is_err());
        assert_eq!(provider.calls.load(Ordering::Relaxed), 0);
    }

    #[cfg(unix)]
    #[test]
    fn evidence_file_is_explicit_new_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private.json");
        let evidence = ReplayEvidence {
            version: "ccr-native-replay-v1".into(),
            origin: ReplayOrigin::RecordedClaim,
            tasks: Vec::new(),
        };
        write_private_evidence(&path, &evidence).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_private_evidence(&path, &evidence).is_err());
    }
}
