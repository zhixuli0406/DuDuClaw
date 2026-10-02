//! Task sandbox: run one delegated task for an agent with
//! `agent.toml [container] sandbox_enabled = true` inside a locked-down
//! container, and hand back only the final reply text.
//!
//! Design: `commercial/docs/DESIGN-task-sandbox-completion-2026-10.md`.
//! Built on the Discovery attempt-container building blocks (reviewed and
//! live-verified): per-family argv / environment / stdin / runtime files and
//! credential documents (`discovery::attempt_adapter`), the host-side stream
//! guard (`discovery::attempt_guard`), the trusted PID-1 supervisor and the
//! bounded subprocess transport (`discovery::process`).
//!
//! Contract:
//! - The AI inside has file and shell tools only: no platform MCP tools
//!   (memory, tasks, channels), no web tools, no sub-agents. Writes stay in a
//!   private, size-capped per-task workspace (a tmpfs) that disappears with
//!   the container; the only output is the reply text. Of the agent
//!   directory it sees only the allowlisted persona/skill/wiki entries
//!   (`container::AGENT_ALLOWLIST`), read-only under `/agent`.
//! - The container needs network to reach the model provider:
//!   `network_access = false` is refused before anything is created.
//! - Fail closed: when the sandbox cannot run, the task fails with a reason
//!   code (`task_sandbox_unavailable`) unless the operator set
//!   `config.toml [container.sandbox] when_unavailable = "run_unsandboxed"`,
//!   which runs it unisolated and audits `task_sandbox_bypassed` every time.
//! - Every string that came out of the CLI has each injected secret value
//!   replaced by `<redacted>` before it leaves this module.
//! - Coverage: only the delegated-task path uses the sandbox. A goal round of
//!   a sandbox-enabled employee always runs Solo (the team gate's first rule),
//!   the Agent Mail arrival trigger is skipped for it, and channel replies,
//!   cron, reminders and the other host paths in [`coverage::HostPath`] keep
//!   running on the host with a once-per-process `task_sandbox_not_applied`
//!   audit event (see [`coverage`]).

pub mod container;
pub mod coverage;
pub mod doctor;
pub mod settings;
pub mod sweep;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use duduclaw_agent::account_rotator::AccountRotator;
use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::types::RuntimeType;
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::discovery::agent_spawn::{
    MAX_UNPARSABLE_LINES, SUPERVISOR_SETUP_FAILED, UNPARSABLE_STREAM, auth_failure, call_cost_for, parse_stream,
    rate_limit_error_envelope, rate_limit_message,
};
use crate::discovery::attempt_adapter::{self as adapter, RuntimeFamily, StreamAdapter};
use crate::discovery::attempt_guard::{GuardVerdict, StreamGuard};
use settings::{SandboxSettings, WhenUnavailable};

pub use coverage::{
    AUDIT_TASK_SANDBOX_NOT_APPLIED, HostAction, HostPath, note_not_applied, sandbox_enabled_in_registry,
};

/// Why the sandbox cannot run this task. `code()` is a closed set used in
/// the `task_sandbox_unavailable` audit event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    InvalidConfig(String),
    UnsupportedRuntime(String),
    NetworkDisabled,
    RootUser,
    UnsupportedPlatform,
    DockerUnreachable,
    ImageMissing(String),
    NoAccount { runtime: &'static str, accepted: String },
}

impl Unavailable {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "invalid_config",
            Self::UnsupportedRuntime(_) => "unsupported_runtime",
            Self::NetworkDisabled => "network_disabled",
            Self::RootUser => "root_user",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::DockerUnreachable => "docker_unreachable",
            Self::ImageMissing(_) => "image_missing",
            Self::NoAccount { .. } => "no_account",
        }
    }

    pub fn message(&self) -> String {
        let text = match self {
            Self::InvalidConfig(why) => format!("the sandbox configuration is invalid: {why}"),
            Self::UnsupportedRuntime(runtime) => format!(
                "runtime `{runtime}` cannot run in the task sandbox (supported: claude, codex, gemini, antigravity, grok, openai_compat)"
            ),
            Self::NetworkDisabled => "the AI inside the sandbox must reach its model provider, but this agent has \
                [container] network_access = false; set network_access = true to use the sandbox"
                .to_string(),
            Self::RootUser => "the gateway runs as root (uid 0); the sandbox refuses to start containers as root".into(),
            Self::UnsupportedPlatform => "the task sandbox needs a unix host with Docker".into(),
            Self::DockerUnreachable => "Docker is not reachable (is the Docker daemon running?)".into(),
            Self::ImageMissing(image) => {
                format!("the sandbox image is not on this machine; run `docker pull {image}` (it is never pulled automatically)")
            }
            Self::NoAccount { runtime, accepted } => {
                format!("no account can run `{runtime}` inside the sandbox; it needs {accepted}")
            }
        };
        format!("Task sandbox unavailable ({}): {text}", self.code())
    }
}

/// Upper bound of an operator-facing sandbox message.
const MESSAGE_MAX_CHARS: usize = 400;

/// A sandbox refusal or failure in a form that may leave the host.
///
/// `message` is built by this module from fixed text plus values the
/// operator configured (image name, account id, limits); it never carries
/// CLI output, Docker stderr or host paths, so the dispatcher may forward it
/// to the requester as is. CLI- or host-derived diagnostics go to `detail`
/// (secret values already replaced by `<redacted>`), which only reaches the
/// host log and the local message-queue record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxFailure {
    /// `true` when the sandbox refused before running anything
    /// (`task_sandbox_unavailable`), `false` when the run itself failed.
    pub unavailable: bool,
    /// Closed set of reason codes (see [`Unavailable::code`] and the
    /// `failure_code` constants below).
    pub code: &'static str,
    pub message: String,
    pub detail: Option<String>,
}

/// Reason codes of failures after the sandbox started working on a task.
pub mod failure_code {
    pub const AGENT_NOT_FOUND: &str = "agent_not_found";
    pub const WORKSPACE_FAILED: &str = "workspace_failed";
    pub const AGENT_DIR_UNMOUNTABLE: &str = "agent_dir_unmountable";
    pub const CONTAINER_REFUSED: &str = "container_refused";
    pub const CONTAINER_CREATE_FAILED: &str = "container_create_failed";
    pub const TRANSPORT_FAILED: &str = "transport_failed";
    pub const CLEANUP_FAILED: &str = "cleanup_failed";
    pub const TOOL_VIOLATION: &str = "tool_violation";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const OUTPUT_OVERFLOW: &str = "output_overflow";
    pub const SUPERVISOR_REFUSED: &str = "supervisor_refused";
    pub const AUTH_FAILED: &str = "auth_failed";
    pub const STEP_LIMIT: &str = "step_limit";
    pub const TIMEOUT: &str = "timeout";
    pub const STOPPED: &str = "stopped";
    pub const NO_REPLY: &str = "no_reply";
}

impl SandboxFailure {
    /// A run failure with an operator-safe message.
    pub fn failed(code: &'static str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            unavailable: false,
            code,
            message: duduclaw_core::truncate_chars(&message, MESSAGE_MAX_CHARS).to_string(),
            detail: None,
        }
    }

    /// Attach a host-only diagnostic (already redacted by the caller).
    pub fn with_detail(self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        let detail = detail.trim();
        Self { detail: (!detail.is_empty()).then(|| detail.to_string()), ..self }
    }

    /// Message plus detail, for the host log and the local queue record.
    pub fn host_text(&self) -> String {
        match &self.detail {
            Some(detail) => format!("{} [{detail}]", self.message),
            None => self.message.clone(),
        }
    }

    /// One `warn!` per refusal / failure: agent, reason code, message and
    /// the redacted detail. Never a credential value.
    pub fn log(&self, agent_id: &str) {
        tracing::warn!(
            agent = %agent_id,
            reason = self.code,
            unavailable = self.unavailable,
            message = %self.message,
            detail = self.detail.as_deref().unwrap_or(""),
            "task sandbox did not complete the task"
        );
    }
}

impl From<&Unavailable> for SandboxFailure {
    fn from(reason: &Unavailable) -> Self {
        Self {
            unavailable: true,
            code: reason.code(),
            message: duduclaw_core::truncate_chars(&reason.message(), MESSAGE_MAX_CHARS).to_string(),
            detail: None,
        }
    }
}

impl std::fmt::Display for SandboxFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.host_text())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// The sandbox cannot run this task at all (fail closed / escape hatch).
    Unavailable(Unavailable),
    /// The sandbox ran and the task failed.
    Failed(SandboxFailure),
}

/// What the dispatcher does next.
#[derive(Debug)]
pub enum SandboxDispatch {
    /// The task ran in the sandbox, or failed closed.
    Completed(Result<String, SandboxFailure>),
    /// `when_unavailable = "run_unsandboxed"`: run it the ordinary way.
    RunUnsandboxed,
}

/// One delegated task as the sandbox sees it.
#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub agent_id: String,
    pub agent_dir: PathBuf,
    pub runtime: RuntimeType,
    pub model: String,
    pub system_prompt: String,
    pub prompt: String,
    pub network_access: bool,
    pub timeout: Duration,
    /// Claude `--disallowedTools` (the only family with a matching flag).
    pub disallowed_tools: Vec<String>,
    /// The operator's explicit `[capabilities] denied_tools` (warned about
    /// for families without a deny flag).
    pub explicit_denied_tools: Vec<String>,
    pub account_pool: Vec<String>,
}

/// The Discovery family that runs `runtime`, if any. Generic print-mode CLIs
/// (qwen, kimi, copilot, …) have none and are unsupported in the sandbox.
pub fn family_for(runtime: RuntimeType) -> Option<RuntimeFamily> {
    match runtime {
        RuntimeType::Claude => Some(RuntimeFamily::Claude),
        RuntimeType::Codex => Some(RuntimeFamily::Codex),
        RuntimeType::Gemini => Some(RuntimeFamily::Gemini),
        RuntimeType::Antigravity => Some(RuntimeFamily::Antigravity),
        RuntimeType::Grok => Some(RuntimeFamily::Grok),
        RuntimeType::OpenAiCompat => Some(RuntimeFamily::OpenAiCompat),
        _ => None,
    }
}

/// Credential kinds that work inside the sandbox, per family (user-facing).
pub fn accepted_credentials(family: RuntimeFamily, provider: &str) -> String {
    match family {
        RuntimeFamily::Claude => "an Anthropic API key, or an OAuth account with a stored token (`claude setup-token`); \
            an account that only lives in the host keychain cannot enter the container"
            .into(),
        RuntimeFamily::Codex => "an OpenAI API key, or an OAuth account whose secret is a Codex auth.json".into(),
        RuntimeFamily::Gemini | RuntimeFamily::Antigravity => "a Gemini API key".into(),
        RuntimeFamily::Grok => "an xAI API key, or an OAuth account whose secret is a Grok auth.json".into(),
        RuntimeFamily::OpenAiCompat => format!("an API key account of provider `{provider}`"),
    }
}

/// Where an `openai_compat` agent's model lives: `provider/model` with a
/// known preset provider, or a bare model on OpenAI.
pub fn openai_compat_target(model: &str) -> Result<(String, String, String), Unavailable> {
    let (provider, wire) = duduclaw_llm::split_model_id(model);
    let provider = provider.unwrap_or("openai");
    let preset = crate::runtime::openai_compat::PROVIDERS.iter().find(|p| p.name == provider).ok_or_else(|| {
        Unavailable::InvalidConfig(format!(
            "openai_compat model `{model}` must be `<provider>/<model>` with a known provider"
        ))
    })?;
    let base = adapter::compatible_endpoint(preset.base_url)
        .map_err(|_| Unavailable::InvalidConfig("provider endpoint is not an https URL".into()))?;
    Ok((preset.name.to_string(), base, wire.to_string()))
}

/// The task prompt with the agent's system prompt (SOUL, IDENTITY) in front
/// as a `<system_instructions>` block. Identical for every family; a closing
/// tag inside the system prompt is escaped so it cannot end the block.
pub fn wrap_prompt(system_prompt: &str, prompt: &str) -> String {
    if system_prompt.trim().is_empty() {
        return prompt.to_string();
    }
    let safe = system_prompt.replace("</system_instructions>", "&lt;/system_instructions&gt;");
    format!("<system_instructions>\n{safe}\n</system_instructions>\n\n{prompt}")
}

/// Every secret value handed to the container: values of the secret
/// variables, plus each string inside a credential document (a CLI may
/// print a token from its own auth.json).
pub fn secret_values(env: &BTreeMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in env {
        if !adapter::is_secret_env(key) || value.trim().is_empty() {
            continue;
        }
        out.push(value.clone());
        if key == adapter::CREDENTIAL_DOC_ENV
            && let Ok(doc) = serde_json::from_str::<Value>(value)
        {
            collect_strings(&doc, &mut out);
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out.dedup();
    out
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        // Short fields (`"type": "oauth"`) are not secrets and would mangle text.
        Value::String(s) if s.len() >= 12 => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(fields) => fields.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

/// Replace every secret value with `<redacted>`, longest first, then anything
/// shaped like a Google API key (generalises the Antigravity setup rule).
/// Apply BEFORE truncating, so a cut never leaves a recognisable prefix.
pub fn redact(text: &str, secrets: &[String]) -> String {
    static GOOGLE_KEY: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let mut out = text.to_string();
    let mut ordered: Vec<&str> = secrets.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in ordered {
        if out.contains(secret) {
            out = out.replace(secret, "<redacted>");
        }
    }
    let re = GOOGLE_KEY.get_or_init(|| regex::Regex::new(r"AIza[0-9A-Za-z_\-]{30,}").expect("static regex"));
    if re.is_match(&out) {
        out = re.replace_all(&out, "<redacted>").into_owned();
    }
    out
}

/// Upper bound of CLI-derived detail inside an error message.
const DETAIL_MAX_CHARS: usize = 600;

fn audit(home: &Path, kind: &str, agent_id: &str, details: Value) {
    crate::security_autopilot::audit_and_emit(
        home,
        &duduclaw_security::audit::AuditEvent::new(kind, agent_id, duduclaw_security::audit::Severity::Warning, details),
    );
}

/// Entry point from `dispatcher::dispatch_sandboxed`.
pub async fn dispatch(
    home: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
) -> SandboxDispatch {
    let spec = {
        let reg = registry.read().await;
        let agent = if agent_id == "default" { reg.main_agent() } else { reg.get(agent_id) };
        let Some(agent) = agent else {
            let failure = SandboxFailure::failed(failure_code::AGENT_NOT_FOUND, "the agent is not registered");
            failure.log(agent_id);
            return SandboxDispatch::Completed(Err(failure));
        };
        let mut parts = Vec::new();
        if let Some(soul) = &agent.soul {
            parts.push(format!("# Soul\n{soul}"));
        }
        if let Some(identity) = &agent.identity {
            parts.push(format!("# Identity\n{identity}"));
        }
        TaskSpec {
            agent_id: if agent_id == "default" { agent.config.agent.name.clone() } else { agent_id.to_string() },
            agent_dir: agent.dir.clone(),
            runtime: RuntimeType::Claude,
            model: agent.config.model.preferred.clone(),
            system_prompt: parts.join("\n\n---\n\n"),
            prompt: prompt.to_string(),
            network_access: agent.config.container.network_access,
            timeout: Duration::from_millis(agent.config.container.timeout_ms),
            disallowed_tools: agent.config.capabilities.disallowed_tools(),
            explicit_denied_tools: agent.config.capabilities.denied_tools.clone(),
            account_pool: agent.config.model.account_pool.clone(),
        }
    };
    let runtime_settings = crate::runtime_config::load_runtime_settings(&spec.agent_dir);
    // O1 confidence-aware delegation routing applies to Claude runtimes only
    // (tier models are Claude ids); identical to the unsandboxed path.
    let model = crate::delegation_router::resolve_delegation_model(
        home,
        &spec.agent_dir,
        &spec.agent_id,
        prompt,
        &spec.model,
        &runtime_settings.utility_model,
        runtime_settings.non_claude_provider().is_none(),
    );
    let spec = TaskSpec { runtime: runtime_settings.provider, model, ..spec };
    let (loaded, when_unavailable) = settings::load(home);
    let result = match loaded {
        Err(why) => Err(SandboxError::Unavailable(Unavailable::InvalidConfig(why))),
        Ok(settings) => {
            let rotator = crate::claude_runner::get_rotator_cached(home).await;
            run(home, &settings, &spec, rotator.as_deref().map_err(String::clone)).await
        }
    };
    settle(home, &spec, when_unavailable, result)
}

/// Map a run result onto the dispatcher's next step, writing the audit
/// events of the fail-closed gate and its escape hatch.
pub fn settle(
    home: &Path,
    spec: &TaskSpec,
    when_unavailable: WhenUnavailable,
    result: Result<String, SandboxError>,
) -> SandboxDispatch {
    match result {
        Ok(text) => SandboxDispatch::Completed(Ok(text)),
        Err(SandboxError::Failed(failure)) => {
            failure.log(&spec.agent_id);
            SandboxDispatch::Completed(Err(failure))
        }
        Err(SandboxError::Unavailable(reason)) => {
            audit(home, "task_sandbox_unavailable", &spec.agent_id, json!({
                "reason": reason.code(), "runtime": spec.runtime.as_str(),
                "when_unavailable": when_unavailable.as_str(),
            }));
            match when_unavailable {
                WhenUnavailable::Fail => {
                    let failure = SandboxFailure::from(&reason);
                    failure.log(&spec.agent_id);
                    SandboxDispatch::Completed(Err(failure))
                }
                WhenUnavailable::RunUnsandboxed => {
                    tracing::warn!(agent = %spec.agent_id, reason = reason.code(),
                        "task sandbox unavailable; running UNSANDBOXED (when_unavailable = run_unsandboxed)");
                    audit(home, "task_sandbox_bypassed", &spec.agent_id, json!({
                        "reason": reason.code(), "runtime": spec.runtime.as_str(),
                    }));
                    SandboxDispatch::RunUnsandboxed
                }
            }
        }
    }
}

/// Checks that need no Docker and no account, in spec order.
pub fn preflight(settings: &SandboxSettings, spec: &TaskSpec) -> Result<RuntimeFamily, Unavailable> {
    let family = family_for(spec.runtime).ok_or_else(|| Unavailable::UnsupportedRuntime(spec.runtime.as_str().into()))?;
    if !spec.network_access {
        return Err(Unavailable::NetworkDisabled);
    }
    #[cfg(unix)]
    if unsafe { libc::geteuid() } == 0 {
        return Err(Unavailable::RootUser);
    }
    #[cfg(not(unix))]
    return Err(Unavailable::UnsupportedPlatform);
    if spec.model.trim().is_empty() {
        return Err(Unavailable::InvalidConfig("the agent has no [model] preferred".into()));
    }
    if spec.timeout.is_zero() {
        return Err(Unavailable::InvalidConfig("the agent's [container] timeout_ms is 0".into()));
    }
    if !settings::valid_image(&settings.image) || !settings::valid_executable(settings.executable(family)) {
        return Err(Unavailable::InvalidConfig("invalid image or executable".into()));
    }
    if family == RuntimeFamily::OpenAiCompat {
        openai_compat_target(&spec.model)?;
    }
    Ok(family)
}

/// One credential that can enter the container, with the container env.
struct Selected {
    account_id: String,
    env: BTreeMap<String, String>,
}

/// Bounded walk over the rotator for `provider`, honouring the agent's
/// `account_pool`: an account without a credential this family can use
/// inside a container (no key, no or an invalid login document) is skipped.
async fn select_account(
    rotator: &AccountRotator,
    family: RuntimeFamily,
    provider: &str,
    pool: &[String],
    base_url: Option<&str>,
) -> Option<Selected> {
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..8 {
        let account = rotator.select_for_provider_with_pool(provider, pool).await?;
        if !seen.insert(account.id.clone()) {
            break;
        }
        let mut selected = account.env_vars.clone();
        if family == RuntimeFamily::OpenAiCompat
            && let Some(key) = duduclaw_core::provider_env::provider_env_key_names(provider)
                .first()
                .and_then(|name| account.env_vars.get(*name))
        {
            selected.insert("OPENAI_API_KEY".into(), key.clone());
        }
        let mut env = adapter::environment(family, &selected);
        if let Some(url) = base_url {
            env.insert("DUDU_ATTEMPT_BASE_URL".into(), url.to_string());
        }
        let Ok(document) = adapter::credential_document(family, account.seat_token.as_deref()) else { continue };
        if !adapter::has_credentials(family, &env, document.as_deref()) {
            continue;
        }
        if let (Some(doc), Some(dest)) = (document, adapter::credential_destination(family)) {
            env.insert(adapter::CREDENTIAL_DOC_ENV.into(), doc);
            env.insert(adapter::CREDENTIAL_DEST_ENV.into(), dest.into());
        }
        return Some(Selected { account_id: account.id, env });
    }
    None
}

/// Run `spec` in the sandbox. `rotator` is the account rotator (or why it
/// could not be loaded).
pub async fn run(
    home: &Path,
    settings: &SandboxSettings,
    spec: &TaskSpec,
    rotator: Result<&AccountRotator, String>,
) -> Result<String, SandboxError> {
    let unavailable = SandboxError::Unavailable;
    let family = preflight(settings, spec).map_err(unavailable)?;
    if !container::docker_reachable().await {
        return Err(unavailable(Unavailable::DockerUnreachable));
    }
    if !container::image_present(&settings.image).await {
        return Err(unavailable(Unavailable::ImageMissing(settings.image.clone())));
    }
    let (provider, base_url, model) = if family == RuntimeFamily::OpenAiCompat {
        let (provider, base, wire) = openai_compat_target(&spec.model).map_err(unavailable)?;
        (provider, Some(base), wire)
    } else {
        (family.provider().to_string(), None, spec.model.clone())
    };
    let no_account = || unavailable(Unavailable::NoAccount {
        runtime: family.name(),
        accepted: accepted_credentials(family, &provider),
    });
    let rotator = rotator.map_err(|why| {
        tracing::warn!(error = %why, "task sandbox: account rotator unavailable");
        no_account()
    })?;
    let selected = select_account(rotator, family, &provider, &spec.account_pool, base_url.as_deref())
        .await
        .ok_or_else(no_account)?;
    if family != RuntimeFamily::Claude && !spec.explicit_denied_tools.is_empty() {
        tracing::warn!(agent = %spec.agent_id, runtime = family.name(),
            "denied_tools has no matching flag on this runtime; the sandbox tool allowlist and the container are the boundary");
    }
    let mut dir = container::TaskDir::create(home).map_err(|e| {
        SandboxError::Failed(
            SandboxFailure::failed(failure_code::WORKSPACE_FAILED, "the sandbox workspace could not be created")
                .with_detail(e.to_string()),
        )
    })?;
    let result = execute(home, settings, spec, family, &model, rotator, &selected, &dir).await;
    if let Err(error) = dir.remove() {
        tracing::warn!(dir = %dir.root().display(), %error, "task sandbox directory could not be removed");
    }
    result
}

/// The `--disallowedTools` value for Claude: entries that are flag-shaped or
/// contain separators are dropped (a deny list can only narrow).
fn claude_disallowed(tools: &[String]) -> Option<String> {
    let kept: Vec<&str> = tools
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty() && !t.starts_with('-') && !t.bytes().any(|c| matches!(c, b',' | b'\n' | b'\r' | 0)))
        .collect();
    (!kept.is_empty()).then(|| kept.join(","))
}

/// Why the event callback stopped the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StopCause {
    StepLimit,
    ToolViolation(String),
    RateLimited,
    TranscriptOverflow,
}

/// Bound of the normalised transcript kept in memory.
const TRANSCRIPT_MAX_BYTES: usize = 1024 * 1024;

#[allow(clippy::too_many_arguments)]
async fn execute(
    home: &Path,
    settings: &SandboxSettings,
    spec: &TaskSpec,
    family: RuntimeFamily,
    model: &str,
    rotator: &AccountRotator,
    selected: &Selected,
    dir: &container::TaskDir,
) -> Result<String, SandboxError> {
    let failed = |code: &'static str, m: String| SandboxError::Failed(SandboxFailure::failed(code, m));
    let failed_with = |code: &'static str, m: String, detail: String| {
        SandboxError::Failed(SandboxFailure::failed(code, m).with_detail(detail))
    };
    let secrets = secret_values(&selected.env);
    let prompt = wrap_prompt(&spec.system_prompt, &spec.prompt);
    let workspace = Path::new(container::WORKSPACE);
    let mut argv = family.argv(model, settings.max_turns, workspace);
    if family == RuntimeFamily::Claude
        && let Some(denied) = claude_disallowed(&spec.disallowed_tools)
    {
        argv.extend(["--disallowedTools".into(), denied]);
    }
    let files = adapter::runtime_files(family, workspace, settings.max_turns, &prompt);
    container::write_config(&dir.config, settings.max_turns, &files)
        .map_err(|e| {
            failed_with(failure_code::WORKSPACE_FAILED, "the sandbox configuration could not be written".into(), e.to_string())
        })?;
    // These errors can name host paths: the detail stays on the host.
    let unmountable = |e: String| {
        failed_with(failure_code::AGENT_DIR_UNMOUNTABLE, "the agent directory cannot be mounted".into(), e)
    };
    let agent_dir = crate::discovery::workspace::canonical_real_directory(&spec.agent_dir)
        .map_err(|e| unmountable(e.to_string()))?;
    let agent_files = container::agent_mounts(&agent_dir).map_err(unmountable)?;
    let home_label = crate::discovery::workspace::canonical_real_directory(home)
        .map(|h| container::home_label(&h))
        .map_err(|e| {
            failed_with(failure_code::WORKSPACE_FAILED, "the gateway home cannot be resolved".into(), e.to_string())
        })?;
    #[cfg(unix)]
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    #[cfg(not(unix))]
    let (uid, gid) = (0u32, 0u32);
    let label_agent: String = spec.agent_id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect();
    let plan = container::ContainerPlan {
        image: &settings.image, executable: settings.executable(family), argv: &argv, env: &selected.env,
        agent_files: &agent_files, config_dir: &dir.config,
        memory_bytes: settings.memory_bytes, pids: settings.pids, cpu_millis: settings.cpu_millis,
        tmp_bytes: settings.tmp_bytes, workspace_bytes: settings.workspace_bytes, home_label: &home_label,
        uid, gid, run_id: &dir.run_id,
        agent_id: if label_agent.is_empty() { "unknown" } else { &label_agent }, timeout: spec.timeout,
    };
    // `build_create` refusals are fixed text ("sandbox container refused: …").
    let (create, name) = container::build_create(&plan)
        .map_err(|why| failed(failure_code::CONTAINER_REFUSED, why))?;
    let payload = adapter::stdin_payload(family, &prompt);

    let mut stream_adapter = StreamAdapter::default();
    let mut guard = StreamGuard::new(family, settings.max_turns);
    let mut transcript = String::new();
    let mut last_text = String::new();
    let mut unparsable = 0u32;
    let mut stop: Option<StopCause> = None;
    let launched = container::launch(create, name, &payload, spec.timeout, |line| {
        if line.iter().all(u8::is_ascii_whitespace) {
            return true;
        }
        // Undecodable output could hide a tool call from the guard.
        let native = match serde_json::from_slice::<Value>(line) {
            Ok(native) if native.is_object() => native,
            _ => {
                unparsable += 1;
                if unparsable > MAX_UNPARSABLE_LINES {
                    stop = Some(StopCause::ToolViolation(UNPARSABLE_STREAM.into()));
                    return false;
                }
                return true;
            }
        };
        let verdict = guard.observe(&native);
        let event = stream_adapter.normalize(family, native);
        if let Some(text) = assistant_text(&event) {
            last_text = text;
        }
        transcript.push_str(&event.to_string());
        transcript.push('\n');
        match verdict {
            GuardVerdict::ToolViolation { tool } => {
                stop = Some(StopCause::ToolViolation(tool));
                return false;
            }
            GuardVerdict::StepLimit => {
                transcript.push_str(&stream_adapter.synthetic_result().to_string());
                transcript.push('\n');
                stop = Some(StopCause::StepLimit);
                return false;
            }
            GuardVerdict::Continue => {}
        }
        if transcript.len() > TRANSCRIPT_MAX_BYTES {
            stop = Some(StopCause::TranscriptOverflow);
            return false;
        }
        if rate_limit_error_envelope(&event) {
            stop = Some(StopCause::RateLimited);
            return false;
        }
        true
    })
    .await;
    // Every exit path consults the guard's final verdict: a forbidden tool
    // still pending when the step limit, a rate limit, an overflow or a
    // transport failure ended the stream is still a violation.
    if !matches!(stop, Some(StopCause::ToolViolation(_)))
        && let GuardVerdict::ToolViolation { tool } = guard.finish()
    {
        stop = Some(StopCause::ToolViolation(tool));
    }
    let summary = parse_stream(&transcript);
    record_usage(spec, family, model, &summary).await;
    if let Some(StopCause::ToolViolation(tool)) = &stop {
        audit(home, "task_sandbox_tool_violation", &spec.agent_id, json!({
            "runtime": family.name(), "tool": tool,
        }));
    }
    let output = match launched {
        Ok(output) => output,
        Err(container::LaunchError::Cleanup(why)) => {
            // The leftover container still carries the credential in its
            // environment: record it (reason code only) so the operator
            // sees it; the periodic sweep removes it once it has stopped
            // or passed its deadline.
            audit(home, "task_sandbox_cleanup_failed", &spec.agent_id, json!({
                "reason": sweep::SweepFailure::ContainerRemoveFailed.code(), "count": 1,
                "runtime": family.name(),
            }));
            return Err(failed_with(
                failure_code::CLEANUP_FAILED,
                "the sandbox container could not be removed; the periodic sweep removes it once it has stopped".into(),
                why,
            ));
        }
        Err(container::LaunchError::Create(stderr)) => {
            // Docker's own stderr can name host mount paths.
            return Err(failed_with(
                failure_code::CONTAINER_CREATE_FAILED,
                "the sandbox container could not be created (docker create failed)".into(),
                detail(&stderr, "", &secrets),
            ));
        }
        Err(container::LaunchError::Transport(why)) => {
            return Err(failed_with(
                failure_code::TRANSPORT_FAILED,
                "the sandbox run failed before the CLI finished".into(),
                detail(&why, "", &secrets),
            ));
        }
    };
    if let Some(StopCause::ToolViolation(tool)) = &stop {
        return Err(failed(
            failure_code::TOOL_VIOLATION,
            format!(
                "the AI in the sandbox used a tool outside the allowed file/shell surface ({}); the task was stopped",
                tool_label(tool)
            ),
        ));
    }
    let failed_run = !output.status.success() || summary.is_error;
    if matches!(stop, Some(StopCause::RateLimited)) || (failed_run && rate_limit_message(&output.stderr)) {
        return Err(failed_with(
            failure_code::RATE_LIMITED,
            format!("the {} provider rate-limited or rejected the request for quota", family.name()),
            detail(&output.stderr, &transcript, &secrets),
        ));
    }
    if matches!(stop, Some(StopCause::TranscriptOverflow)) {
        return Err(failed(failure_code::OUTPUT_OVERFLOW, "the sandbox output exceeded 1 MiB and was stopped".into()));
    }
    if output.status.code() == Some(SUPERVISOR_SETUP_FAILED) && transcript.is_empty() && unparsable == 0 {
        return Err(failed_with(
            failure_code::SUPERVISOR_REFUSED,
            "the sandbox supervisor refused its setup".into(),
            detail(&output.stderr, "", &secrets),
        ));
    }
    let worked = used_a_tool(&transcript);
    if !worked && auth_failure(&transcript, &output.stderr) {
        rotator.on_error(&selected.account_id).await;
        return Err(failed_with(
            failure_code::AUTH_FAILED,
            format!(
                "authentication failed: the {} provider rejected the credential of account `{}`",
                family.name(),
                selected.account_id
            ),
            detail(&output.stderr, &transcript, &secrets),
        ));
    }
    let reply = summary.final_text.as_deref().map(str::trim).filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| (!last_text.trim().is_empty()).then(|| last_text.trim().to_string()));
    let step_limit = matches!(stop, Some(StopCause::StepLimit)) || max_turns_result(&transcript);
    if step_limit {
        return match reply {
            Some(text) => Ok(redact(&text, &secrets)),
            None => Err(failed(
                failure_code::STEP_LIMIT,
                format!("the AI reached the sandbox step limit ({}) before writing a reply", settings.max_turns),
            )),
        };
    }
    if output.timed_out {
        return Err(failed(
            failure_code::TIMEOUT,
            format!("the sandbox task timed out after {} s", spec.timeout.as_secs()),
        ));
    }
    if output.stopped {
        return Err(failed(failure_code::STOPPED, "the sandbox run was stopped".into()));
    }
    match reply {
        Some(text) if !failed_run => {
            let cost = call_cost_for(family, summary.model.as_deref().unwrap_or(model), &summary);
            let cents = if cost.usd.is_finite() && cost.usd >= 0.0 { (cost.usd * 100.0).ceil() as u64 } else { 0 };
            rotator.on_success(&selected.account_id, cents).await;
            Ok(redact(&text, &secrets))
        }
        _ => Err(failed_with(
            failure_code::NO_REPLY,
            format!(
                "the {} CLI in the sandbox ended without a reply (exit {})",
                family.name(),
                output.status.code().map_or_else(|| "signal".to_string(), |c| c.to_string())
            ),
            detail(&output.stderr, &transcript, &secrets),
        )),
    }
}

/// A tool name as the AI wrote it, reduced to a short identifier: the name
/// comes from the model's own stream and is shown to the requester.
fn tool_label(tool: &str) -> String {
    let kept: String = tool
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        .collect();
    let kept = duduclaw_core::truncate_chars(&kept, 64).to_string();
    if kept.is_empty() { "unnamed tool".into() } else { kept }
}

/// The text of a normalised assistant event, when it has any.
fn assistant_text(event: &Value) -> Option<String> {
    if event["type"].as_str() != Some("assistant") {
        return None;
    }
    let text: String = event["message"]["content"]
        .as_array()?
        .iter()
        .filter(|b| b["type"].as_str() == Some("text"))
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

/// Whether the normalised transcript shows the CLI ran at least one tool (an
/// assistant event with a `tool_use` content block): a run that worked
/// cannot have been rejected for its credential.
fn used_a_tool(transcript: &str) -> bool {
    transcript.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).any(|e| {
        e["type"].as_str() == Some("assistant")
            && e["message"]["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|b| b["type"].as_str() == Some("tool_use")))
    })
}

/// A native `--max-turns` stop (Claude / Grok `error_max_turns`).
fn max_turns_result(transcript: &str) -> bool {
    transcript
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|e| e["type"].as_str() == Some("result") && e["subtype"].as_str() == Some("error_max_turns"))
}

/// Readable diagnostic: the CLI's own error fields plus the stderr tail,
/// redacted first, then bounded.
pub fn detail(stderr: &str, transcript: &str, secrets: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for event in transcript.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let fields: &[&str] = match event["type"].as_str() {
            Some("result") if event["is_error"].as_bool() == Some(true) => &["result", "error", "errors"],
            Some("error") => &["error", "message"],
            _ => continue,
        };
        for field in fields {
            match event.get(*field) {
                None | Some(Value::Null) => {}
                Some(Value::String(s)) if s.trim().is_empty() => {}
                Some(Value::String(s)) => parts.push(s.clone()),
                Some(other) => parts.push(other.to_string()),
            }
        }
    }
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        let chars: Vec<char> = stderr.chars().collect();
        let tail: String = chars[chars.len().saturating_sub(2000)..].iter().collect();
        parts.push(tail);
    }
    let joined = redact(&parts.join(" | "), secrets);
    let joined = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() {
        return "no diagnostic output".into();
    }
    duduclaw_core::truncate_chars(&joined, DETAIL_MAX_CHARS).to_string()
}

async fn record_usage(spec: &TaskSpec, _family: RuntimeFamily, model: &str, summary: &crate::discovery::agent_spawn::StreamSummary) {
    let Some(usage) = &summary.usage else { return };
    if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
        telemetry
            .record(&spec.agent_id, crate::cost_telemetry::RequestType::Dispatch, summary.model.as_deref().unwrap_or(model), usage)
            .await;
    }
}

#[cfg(test)]
mod tests;
#[cfg(all(test, unix))]
mod tests_runner;
#[cfg(all(test, unix))]
mod tests_docker;
