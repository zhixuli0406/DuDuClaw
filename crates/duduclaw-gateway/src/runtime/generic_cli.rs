//! Generic print-mode runtime — drives ANY catalog CLI that answers one
//! non-interactive turn on stdout (WP-B, `docs/todo/TODO-ai-runtimes-2026-09.md` §3).
//!
//! `claude` / `codex` / `gemini` / `antigravity` / `grok` keep their bespoke
//! modules: each of those has real per-vendor wiring (account rotation, MCP
//! config injection, capability→sandbox-flag translation, PTY recovery) that
//! is not shared. Everything else — Qwen Code, Kimi Code, GitHub Copilot CLI,
//! Kiro CLI, Cursor, Mistral Vibe, OpenCode — is a print-mode CLI with the
//! same three questions (which argv, which output format, which env), and
//! those three answers live in [`duduclaw_core::runtime_catalog`]. This module
//! is the one implementation that reads them.
//!
//! ## Capability enforcement (READ THIS)
//!
//! A print-mode CLI cannot show approval prompts, so every one of these
//! vendors requires an auto-approve flag or the run narrates and exits without
//! calling anything. Those flags are recorded in each entry's
//! `headless.args_template` and are therefore always applied.
//!
//! The hard, fail-closed confinement for this runtime is the **native OS
//! sandbox** ([`super::apply_native_sandbox`], opt-in via `[capabilities]
//! native_sandbox = true`) — identical to `runtime/grok.rs`, which pairs
//! `--permission-mode bypassPermissions` with a capability-derived profile.
//! Unlike grok, this module has no per-CLI `--tools` translation, so when an
//! agent declares tool restrictions it CANNOT honour them: it emits a
//! structured `warn!` per spawn, which is exactly the contract the project
//! already applies to Antigravity ("has no equivalent flag and warns per
//! spawn"). Do not silently drop the restriction; do not pretend it applied.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use duduclaw_core::runtime_catalog::{OutputFormat, RuntimeSpec, PROMPT_PLACEHOLDER};

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Hard backstop on the whole subprocess — same value the bespoke CLI
/// runtimes use, so a slow vendor behaves the same whichever module drives it.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Cap the system prompt embedded into the prompt payload (ARG_MAX safety).
/// Matches `runtime/grok.rs`.
const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;

/// Bytes of stderr echoed back in an error message. Enough to diagnose, small
/// enough that a runaway CLI can't blow up a channel reply.
const STDERR_TAIL_BYTES: usize = 300;

// ── Typed failures ───────────────────────────────────────────────────────

/// What went wrong in one headless run.
///
/// `AgentRuntime::execute` is `Result<_, String>`, so these are rendered to
/// text at the boundary — but the *wording* is load-bearing:
/// `channel_reply::classify_cli_failure` routes on it, so each variant's
/// `Display` deliberately contains that classifier's marker
/// (`not logged in` ⇒ `AuthFailed`, `hard timeout` ⇒ `Timeout`,
/// `empty response` ⇒ `EmptyResponse`, `spawn error` ⇒ `SpawnError`). Keeping
/// the enum means the mapping is stated once and unit-tested, instead of
/// being re-improvised at each `return Err(format!(…))`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenericCliError {
    /// The CLI is installed but has no usable credentials.
    AuthRequired {
        runtime: &'static str,
        /// The operator action that fixes it (login command / API key env).
        action: String,
        stderr_tail: String,
    },
    /// Non-zero exit that is not an auth failure.
    NonZeroExit {
        runtime: &'static str,
        code: i32,
        stderr_tail: String,
    },
    /// Exit 0 with nothing usable on stdout. A silent success is worse than a
    /// failure: every channel skips an empty send, so the turn would vanish
    /// and an empty assistant message would be appended to the session.
    EmptyOutput {
        runtime: &'static str,
        stderr_tail: String,
    },
    Timeout {
        runtime: &'static str,
        secs: u64,
    },
    Spawn {
        runtime: &'static str,
        error: String,
    },
    /// Output arrived but no assistant text could be recovered from it.
    Unparseable {
        runtime: &'static str,
        format: &'static str,
        stdout_tail: String,
    },
}

impl std::fmt::Display for GenericCliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthRequired { runtime, action, stderr_tail } => write!(
                f,
                "{runtime} CLI is not logged in (authentication required): {action}. \
                 stderr tail: {stderr_tail}"
            ),
            Self::NonZeroExit { runtime, code, stderr_tail } => {
                write!(f, "{runtime} CLI exited with {code}: {stderr_tail}")
            }
            Self::EmptyOutput { runtime, stderr_tail } => write!(
                f,
                "Empty response from {runtime} CLI (exit 0); stderr tail: {stderr_tail}"
            ),
            Self::Timeout { runtime, secs } => {
                write!(f, "{runtime} CLI hard timeout after {secs}s")
            }
            Self::Spawn { runtime, error } => {
                write!(f, "spawn error: failed to start {runtime} CLI: {error}")
            }
            Self::Unparseable { runtime, format, stdout_tail } => write!(
                f,
                "Empty response from {runtime} CLI: no assistant text in its {format} \
                 output; stdout tail: {stdout_tail}"
            ),
        }
    }
}

impl From<GenericCliError> for String {
    fn from(e: GenericCliError) -> Self {
        e.to_string()
    }
}

/// Vendor-neutral "you are not signed in" fingerprints.
///
/// Matched case-insensitively at word/phrase boundaries via
/// [`duduclaw_core::word_contains_ci`] — never an unanchored `contains` — so a
/// normal answer that merely discusses authentication as a topic cannot trip
/// the classifier. Distilled from the per-vendor list already proven in
/// `runtime/grok.rs` plus the conventional CLI/HTTP auth vocabulary.
const AUTH_FAILURE_PATTERNS: &[&str] = &[
    "not signed in",
    "not signed-in",
    "not logged in",
    "not authenticated",
    "unauthenticated",
    "unauthorized",
    "login required",
    "please log in",
    "please login",
    "you must log in",
    "run `login`",
    "authentication failed",
    "authentication required",
    "auth failed",
    "invalid api key",
    "invalid token",
    "invalid credentials",
    "missing api key",
    "missing credentials",
    "no api key",
    "api key not",
    "token expired",
    "expired token",
    "session expired",
    "401",
];

/// True when `text` carries a known not-authenticated fingerprint.
pub fn looks_like_auth_failure(text: &str) -> bool {
    AUTH_FAILURE_PATTERNS
        .iter()
        .any(|p| duduclaw_core::word_contains_ci(text, p))
}

/// The operator action that fixes an auth failure for this runtime, built from
/// the catalog so it names the real login command / env var rather than a
/// generic "check your credentials".
fn auth_action(spec: &RuntimeSpec) -> String {
    let login = spec.auth.login.args();
    let mut parts = Vec::new();
    if !login.is_empty() {
        parts.push(format!("執行 `{} {}`", spec.binary, login.join(" ")));
    }
    if let Some(env) = spec.auth.api_key_env {
        parts.push(format!("或設定有效的 {env}"));
    }
    if parts.is_empty() {
        format!("請確認 {} 的憑證設定（{}）", spec.display_name, spec.vendor_url)
    } else {
        format!(
            "請在執行 gateway 的環境（若為 Docker 需進入容器）{}",
            parts.join(" ")
        )
    }
}

// ── Output parsing ───────────────────────────────────────────────────────

/// Pull the assistant's text out of ONE JSON value, shape-agnostically.
///
/// Every vendor invents its own envelope, but they all bottom out in one of a
/// handful of shapes, so this walks them in priority order rather than
/// hard-coding a per-vendor path (which would need a new branch for every
/// runtime added to the catalog — the exact duplication WP-B removes):
///   1. a bare string
///   2. `result` — Qwen's and Cursor's `{"type":"result","result":"…"}`
///      terminal object
///   3. `response` / `text` — Gemini-family envelopes
///   4. `content` — a string (Kimi's OpenAI-chat shape) or a
///      `[{"type":"text","text":"…"}]` block array (Anthropic shape, and
///      Mistral Vibe's history entries)
///   5. `message` / `delta` / `part` — recurse into the nested envelope
///      (`part` is OpenCode's `{"type":"text","part":{"text":"…"}}`)
///   6. `choices[0]` — OpenAI chat completions
fn extract_text(v: &Value) -> Option<String> {
    fn non_empty(s: &str) -> Option<String> {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    }
    match v {
        Value::String(s) => non_empty(s),
        Value::Object(map) => {
            for key in ["result", "response", "text"] {
                if let Some(Value::String(s)) = map.get(key)
                    && let Some(t) = non_empty(s)
                {
                    return Some(t);
                }
            }
            if let Some(content) = map.get("content") {
                match content {
                    Value::String(s) => {
                        if let Some(t) = non_empty(s) {
                            return Some(t);
                        }
                    }
                    Value::Array(blocks) => {
                        let joined: String = blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("");
                        if let Some(t) = non_empty(&joined) {
                            return Some(t);
                        }
                    }
                    _ => {}
                }
            }
            for key in ["message", "delta", "part"] {
                if let Some(nested) = map.get(key)
                    && let Some(t) = extract_text(nested)
                {
                    return Some(t);
                }
            }
            map.get("choices")
                .and_then(Value::as_array)
                .and_then(|c| c.first())
                .and_then(extract_text)
        }
        // A bare array — Qwen's buffered `--output-format json` and Mistral
        // Vibe's `--output json` are both an array of events / history
        // entries. Scan from the END, because the answer is the last thing
        // said; and prefer an entry that identifies itself as an assistant
        // message, so a trailing tool-result entry (which also carries a
        // `content` block array) is not mistaken for the reply. Fall back to
        // an unfiltered reverse scan rather than returning nothing, for
        // vendors whose entries carry no discriminator at all.
        Value::Array(items) => items
            .iter()
            .rev()
            .filter(|e| is_assistant_event(e))
            .find_map(extract_text)
            .or_else(|| items.iter().rev().find_map(extract_text)),
        _ => None,
    }
}

/// Is this event an assistant message (as opposed to a tool result, a
/// progress/meta frame, or a user echo)?
///
/// Permissive by design: an event with NO role/type discriminator at all is
/// treated as a candidate, because a vendor that ships a bare
/// `{"content": "…"}` line would otherwise be silently unreadable. An event
/// that explicitly says it is something else is always rejected.
fn is_assistant_event(v: &Value) -> bool {
    let Some(map) = v.as_object() else {
        return true;
    };
    for key in ["role", "type", "subtype"] {
        if let Some(Value::String(kind)) = map.get(key) {
            let k = kind.to_ascii_lowercase();
            if k == "assistant" || k == "success" {
                return true;
            }
            // A recognised NON-assistant discriminator disqualifies it.
            if matches!(
                k.as_str(),
                "tool" | "tool_result" | "tool_use" | "user" | "system" | "meta"
                    | "error" | "progress" | "thinking" | "reasoning"
            ) {
                return false;
            }
        }
    }
    true
}

/// The terminal `{"type":"result", …, "result":"…"}` object, if this event is
/// one. Highest-priority answer: a CLI that emits it is telling us verbatim
/// what its final answer was, so it beats "the last assistant chunk".
fn terminal_result(v: &Value) -> Option<String> {
    let map = v.as_object()?;
    if map.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    let s = map.get("result").and_then(Value::as_str)?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Fold a JSONL / stream-json transcript into the final assistant text.
pub fn parse_jsonl(stdout: &str) -> Option<String> {
    let mut terminal = None;
    let mut last_assistant = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A CLI that interleaves plain log lines with JSON must not abort the
        // fold — skip what doesn't parse.
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(t) = terminal_result(&v) {
            terminal = Some(t);
            continue;
        }
        if is_assistant_event(&v)
            && let Some(t) = extract_text(&v)
        {
            last_assistant = Some(t);
        }
    }
    terminal.or(last_assistant)
}

/// Extract the answer from a headless run's stdout per the spec's format.
pub fn parse_output(stdout: &str, format: OutputFormat) -> Option<String> {
    match format {
        OutputFormat::Text => {
            let t = stdout.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
        OutputFormat::Json => serde_json::from_str::<Value>(stdout.trim())
            .ok()
            .as_ref()
            .and_then(extract_text)
            // A CLI that advertises `json` but emitted a stream anyway (or a
            // single object per line) still yields its answer rather than a
            // spurious "unparseable".
            .or_else(|| parse_jsonl(stdout)),
        OutputFormat::Jsonl => parse_jsonl(stdout),
    }
}

// ── Prompt payload ───────────────────────────────────────────────────────

/// Build the prompt payload: system instructions + history + user message, all
/// embedded as text.
///
/// Same construction as `runtime/grok.rs::build_prompt`, and for the same
/// reason: every one of these CLIs has a different (or no) system-prompt flag,
/// and several *replace* their own agent scaffolding when given one. Embedding
/// the instructions in the payload delivers them everywhere, identically.
/// Pure, so it is unit-testable without spawning.
pub fn build_prompt(context: &RuntimeContext, user_prompt: &str, runtime: &str) -> String {
    let system_prompt: &str = if context.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        warn!(
            runtime,
            agent = %context.agent_id,
            original_len = context.system_prompt.len(),
            "system_prompt truncated to 64KB"
        );
        duduclaw_core::truncate_bytes(&context.system_prompt, MAX_SYSTEM_PROMPT_BYTES)
    } else {
        &context.system_prompt
    };

    // Argument injection guard: a prompt starting with '-' would be parsed as
    // a flag by every one of these CLIs.
    let safe_prompt = if user_prompt.starts_with('-') {
        format!(" {user_prompt}")
    } else {
        user_prompt.to_string()
    };

    let with_history = if context.conversation_history.is_empty() {
        safe_prompt
    } else {
        super::format_history_as_prompt(&context.conversation_history, &safe_prompt)
    };

    if system_prompt.is_empty() {
        with_history
    } else {
        let safe_system =
            system_prompt.replace("</system_instructions>", "&lt;/system_instructions&gt;");
        format!("<system_instructions>\n{safe_system}\n</system_instructions>\n\n{with_history}")
    }
}

/// Expand the catalog's argv template: [`PROMPT_PLACEHOLDER`] becomes the
/// payload (one argv element), the model flag is appended when a model is
/// configured, and everything else passes through verbatim.
///
/// Pure so the exact command line is unit-testable without a binary.
pub fn build_args(spec: &RuntimeSpec, payload: &str, model: &str) -> Vec<String> {
    let mut args: Vec<String> = spec
        .headless
        .args_template
        .iter()
        .map(|a| {
            if *a == PROMPT_PLACEHOLDER {
                payload.to_string()
            } else {
                (*a).to_string()
            }
        })
        .collect();
    args.extend(spec.headless.model_args(model));
    args
}

// ── The runtime ──────────────────────────────────────────────────────────

/// One catalog CLI, driven in print mode.
pub struct GenericCliRuntime {
    spec: &'static RuntimeSpec,
    program: String,
}

impl GenericCliRuntime {
    /// Build a runtime for `spec` if its CLI is installed. `None` ⇒ not
    /// present on this host, so `RuntimeRegistry` must not register it.
    pub fn detect(spec: &'static RuntimeSpec, user_home: &Path) -> Option<Self> {
        let program = duduclaw_core::detect_runtime(spec.id, user_home)?;
        Some(Self { spec, program })
    }

    /// Bind a spec to an explicit program path. Used by the fake-binary
    /// integration tests, and by any caller that already resolved the path.
    pub fn with_program(spec: &'static RuntimeSpec, program: impl Into<String>) -> Self {
        Self { spec, program: program.into() }
    }

    pub fn spec(&self) -> &'static RuntimeSpec {
        self.spec
    }

    /// Run one turn. Split out from the trait method so the error type stays
    /// typed all the way to the boundary.
    async fn run(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, GenericCliError> {
        let id = self.spec.id;
        let payload = build_prompt(context, prompt, id);
        let args = build_args(self.spec, &payload, &context.model);

        // Capability honesty (see the module doc): this runtime has no per-CLI
        // tool-confinement flag to translate into, so a declared restriction
        // is reported, never silently dropped.
        if let Some(caps) = context.capabilities.as_ref()
            && caps.has_tool_restrictions()
        {
            warn!(
                runtime = id,
                agent = %context.agent_id,
                level = ?duduclaw_core::types::sandbox_level_for(context.capabilities.as_ref()),
                "agent declares tool restrictions but the {id} CLI exposes no \
                 confinement flag this runtime can translate — only the opt-in \
                 native OS sandbox ([capabilities] native_sandbox) enforces them here"
            );
        }

        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&args);

        if let Some(dir) = context.agent_dir.as_deref() {
            cmd.current_dir(dir);
        }

        // Home integrity: the gateway frequently runs with a `$HOME` that is
        // not the user's (launchd, systemd, Docker), and every one of these
        // CLIs looks for its credentials under `$HOME`. Reuse the probe the
        // grok runtime already proved out.
        let user_home =
            super::grok::resolve_user_home(&context.home_dir, std::env::var("HOME").ok().as_deref());
        for (k, v) in super::grok::build_home_env(&user_home, None) {
            cmd.env(k, v);
        }

        for (k, v) in self.spec.headless.extra_env {
            cmd.env(k, v);
        }

        // Model selection for a CLI with no `--model` flag at all (Mistral
        // Vibe: `VIBE_ACTIVE_MODEL`). When a runtime has neither a flag nor a
        // variable (Kiro selects its model through `kiro-cli settings`, not
        // per call), say so once rather than letting the caller believe the
        // agent's configured model was honoured.
        match self.spec.headless.model_env_pair(&context.model) {
            Some((name, value)) => {
                cmd.env(name, value);
            }
            None if self.spec.headless.model_flag.is_none() && !context.model.trim().is_empty() => {
                warn!(
                    runtime = id,
                    agent = %context.agent_id,
                    model = %context.model,
                    "{} exposes no per-call model selection — the CLI's own \
                     configured model is what will answer",
                    self.spec.display_name
                );
            }
            None => {}
        }
        // Colour codes and progress spinners are noise in captured stdout and
        // break JSON parsing outright.
        cmd.env("NO_COLOR", "1").env("TERM", "dumb");

        // API key: forwarded only when the gateway's own environment has it.
        // Never synthesized, never read from a config file here.
        if let Some(env_name) = self.spec.auth.api_key_env
            && let Ok(key) = std::env::var(env_name)
            && !key.is_empty()
        {
            cmd.env(env_name, key);
        }

        // MCP identity, for CLIs that spawn the duduclaw MCP server as a child
        // and inherit this env.
        if self.spec.mcp {
            for (k, v) in duduclaw_core::agent_identity_env_vars_default(&context.agent_id) {
                cmd.env(k, v);
            }
            for (k, v) in duduclaw_core::mcp_forward_env_vars() {
                cmd.env(k, v);
            }
        }

        let stdin_prompt = self.spec.headless.prompt_via_stdin();
        cmd.stdin(if stdin_prompt {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

        // The hard, fail-closed confinement (opt-in). Must run last, right
        // before spawn, and its Err must abort the spawn.
        super::apply_native_sandbox(&mut cmd, context.capabilities.as_ref(), context.agent_dir.as_deref(), id)
            .map_err(|e| GenericCliError::Spawn { runtime: id, error: e })?;

        info!(
            runtime = id,
            agent = %context.agent_id,
            model = %context.model,
            output = self.spec.headless.output.as_str(),
            stdin_prompt,
            "generic print-mode runtime: spawning"
        );

        let mut child = cmd
            .spawn()
            .map_err(|e| GenericCliError::Spawn { runtime: id, error: e.to_string() })?;

        if stdin_prompt && let Some(mut sink) = child.stdin.take() {
            // Best effort: a CLI that closes stdin early (it already has the
            // whole prompt) must not turn into a broken-pipe failure.
            let _ = sink.write_all(payload.as_bytes()).await;
            let _ = sink.shutdown().await;
        }

        let output = tokio::time::timeout(
            Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| GenericCliError::Timeout { runtime: id, secs: DEFAULT_TIMEOUT_SECS })?
        .map_err(|e| GenericCliError::Spawn { runtime: id, error: e.to_string() })?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr_tail = duduclaw_core::truncate_bytes(stderr.trim(), STDERR_TAIL_BYTES).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout);

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            if looks_like_auth_failure(&stderr) || looks_like_auth_failure(&stdout) {
                warn!(runtime = id, agent = %context.agent_id, code, "CLI reported an authentication failure");
                return Err(GenericCliError::AuthRequired {
                    runtime: id,
                    action: auth_action(self.spec),
                    stderr_tail,
                });
            }
            return Err(GenericCliError::NonZeroExit { runtime: id, code, stderr_tail });
        }

        // Exit 0 with nothing on stdout: check for an auth failure hiding
        // behind the silence before reporting it as an empty response.
        if stdout.trim().is_empty() {
            if looks_like_auth_failure(&stderr) {
                return Err(GenericCliError::AuthRequired {
                    runtime: id,
                    action: auth_action(self.spec),
                    stderr_tail,
                });
            }
            return Err(GenericCliError::EmptyOutput { runtime: id, stderr_tail });
        }

        let Some(content) = parse_output(&stdout, self.spec.headless.output) else {
            if looks_like_auth_failure(&stdout) || looks_like_auth_failure(&stderr) {
                return Err(GenericCliError::AuthRequired {
                    runtime: id,
                    action: auth_action(self.spec),
                    stderr_tail,
                });
            }
            return Err(GenericCliError::Unparseable {
                runtime: id,
                format: self.spec.headless.output.as_str(),
                stdout_tail: duduclaw_core::truncate_bytes(stdout.trim(), STDERR_TAIL_BYTES).to_string(),
            });
        };

        // No vendor here exposes a confirmed usage-stats schema, so tokens are
        // ESTIMATED with the gateway's CJK-aware heuristic. They feed
        // CostTelemetry as approximations, which is stated rather than
        // presented as a measured count.
        let input_tokens = crate::prompt_compression::estimate_tokens(&payload);
        let output_tokens = crate::prompt_compression::estimate_tokens(&content);

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: id.to_string(),
        })
    }
}

#[async_trait]
impl AgentRuntime for GenericCliRuntime {
    fn name(&self) -> &str {
        self.spec.id
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        self.run(prompt, context).await.map_err(Into::into)
    }

    async fn is_available(&self) -> bool {
        // Path check only — deliberately NOT a `--version` spawn. Several of
        // these CLIs start their TUI when invoked without a recognised flag,
        // and the registry builds this for every catalog entry at startup.
        !self.program.is_empty() && Path::new(&self.program).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::runtime_catalog::spec_for;
    use std::path::PathBuf;

    fn ctx(dir: &Path) -> RuntimeContext {
        RuntimeContext {
            agent_dir: Some(dir.to_path_buf()),
            system_prompt: "you are a test".to_string(),
            model: "test-model".to_string(),
            max_tokens: 1024,
            home_dir: dir.to_path_buf(),
            agent_id: "tester".to_string(),
            preferred_provider: None,
            conversation_history: Vec::new(),
            capabilities: None,
            account_pool: Vec::new(),
        }
    }

    /// Write an executable shell script that plays the part of a vendor CLI.
    fn fake_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    // ── pure helpers ────────────────────────────────────────────────────

    #[test]
    fn build_args_substitutes_the_prompt_and_appends_the_model() {
        let spec = spec_for("copilot").unwrap();
        let args = build_args(spec, "HELLO", "gpt-5.4");
        assert!(args.contains(&"HELLO".to_string()), "prompt substituted: {args:?}");
        assert!(
            !args.iter().any(|a| a == PROMPT_PLACEHOLDER),
            "placeholder must not survive: {args:?}"
        );
        assert!(
            args.contains(&"--model=gpt-5.4".to_string()),
            "copilot documents the JOINED --model= form: {args:?}"
        );
    }

    #[test]
    fn build_args_omits_the_model_flag_when_no_model_is_set() {
        let spec = spec_for("qwen").unwrap();
        let args = build_args(spec, "x", "");
        assert!(!args.iter().any(|a| a.starts_with("--model")), "{args:?}");
    }

    #[test]
    fn build_prompt_frames_system_and_guards_leading_dash() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        let p = build_prompt(&c, "--not-a-flag", "test");
        assert!(p.contains("<system_instructions>"));
        assert!(
            p.trim_start_matches(|ch| ch != '-').starts_with("--not-a-flag"),
            "leading dash is neutralised by a space, not stripped: {p}"
        );
    }

    #[test]
    fn extract_text_walks_every_known_envelope() {
        let cases: &[(&str, &str)] = &[
            (r#""bare string""#, "bare string"),
            (r#"{"result":"final"}"#, "final"),
            (r#"{"response":"r"}"#, "r"),
            (r#"{"role":"assistant","content":"kimi shape"}"#, "kimi shape"),
            (
                r#"{"message":{"content":[{"type":"text","text":"anthropic shape"}]}}"#,
                "anthropic shape",
            ),
            (
                r#"{"choices":[{"message":{"content":"openai shape"}}]}"#,
                "openai shape",
            ),
            (r#"[{"type":"assistant"},{"type":"result","result":"last wins"}]"#, "last wins"),
            // OpenCode NDJSON event: the text hides under `.part.text`.
            (r#"{"type":"text","part":{"text":"opencode shape"}}"#, "opencode shape"),
            // Cursor `--output-format json`: a single object with `.result`.
            (r#"{"type":"result","result":"cursor shape","duration_ms":12}"#, "cursor shape"),
        ];
        for (json, want) in cases {
            let v: Value = serde_json::from_str(json).unwrap();
            assert_eq!(extract_text(&v).as_deref(), Some(*want), "{json}");
        }
        assert!(extract_text(&serde_json::json!({"unrelated": 1})).is_none());
        assert!(extract_text(&serde_json::json!({"content": "   "})).is_none());
    }

    /// Mistral Vibe's `--output json` prints the WHOLE history array — the
    /// answer is the last `type == "message" && role == "assistant"` entry,
    /// and a trailing tool entry (which also carries a `content` block array)
    /// must not be mistaken for it.
    #[test]
    fn extract_text_from_a_history_array_skips_trailing_tool_entries() {
        let history = serde_json::json!([
            {"type": "message", "role": "user", "content": [{"type": "text", "text": "hi"}]},
            {"type": "message", "role": "assistant",
             "content": [{"type": "text", "text": "the vibe answer"}]},
            {"type": "tool_result", "content": [{"type": "text", "text": "tool noise"}]}
        ]);
        assert_eq!(
            extract_text(&history).as_deref(),
            Some("the vibe answer"),
            "the trailing tool entry must not win"
        );
    }

    /// OpenCode's `--format json` is NDJSON with the text in `type:"text"`
    /// events; `error` events must never be returned as the answer.
    #[test]
    fn parse_jsonl_reads_opencode_part_text_and_ignores_errors() {
        let out = concat!(
            r#"{"type":"step_start"}"#,
            "\n",
            r#"{"type":"text","part":{"text":"opencode answer"}}"#,
            "\n",
            r#"{"type":"error","part":{"text":"should be ignored"}}"#,
            "\n"
        );
        assert_eq!(parse_jsonl(out).as_deref(), Some("opencode answer"));
    }

    #[test]
    fn parse_jsonl_prefers_the_terminal_result_over_the_last_chunk() {
        let out = concat!(
            r#"{"type":"assistant","message":{"content":[{"text":"partial"}]}}"#,
            "\n",
            r#"{"type":"result","subtype":"success","result":"the answer"}"#,
            "\n"
        );
        assert_eq!(parse_jsonl(out).as_deref(), Some("the answer"));
    }

    #[test]
    fn parse_jsonl_falls_back_to_the_last_assistant_line() {
        let out = concat!(
            r#"{"role":"assistant","content":"first"}"#,
            "\n",
            r#"{"role":"tool","tool_call_id":"t1","content":"tool noise"}"#,
            "\n",
            r#"{"role":"assistant","content":"second"}"#,
            "\n"
        );
        assert_eq!(
            parse_jsonl(out).as_deref(),
            Some("second"),
            "tool lines must not be mistaken for the answer"
        );
    }

    #[test]
    fn parse_jsonl_skips_non_json_log_noise() {
        let out = "starting up…\n{\"role\":\"assistant\",\"content\":\"ok\"}\nbye\n";
        assert_eq!(parse_jsonl(out).as_deref(), Some("ok"));
    }

    #[test]
    fn parse_output_handles_each_format() {
        assert_eq!(
            parse_output("  plain  \n", OutputFormat::Text).as_deref(),
            Some("plain")
        );
        assert_eq!(
            parse_output(r#"[{"type":"result","result":"qwen"}]"#, OutputFormat::Json).as_deref(),
            Some("qwen")
        );
        assert_eq!(
            parse_output("{\"role\":\"assistant\",\"content\":\"k\"}\n", OutputFormat::Jsonl)
                .as_deref(),
            Some("k")
        );
        assert!(parse_output("", OutputFormat::Text).is_none());
        assert!(parse_output("not json at all", OutputFormat::Jsonl).is_none());
    }

    #[test]
    fn auth_failure_detection_does_not_fire_on_ordinary_prose() {
        assert!(looks_like_auth_failure("Error: not logged in. Run `kimi login`."));
        assert!(looks_like_auth_failure("HTTP 401 Unauthorized"));
        assert!(!looks_like_auth_failure(
            "Here is how OAuth authentication works in your codebase."
        ));
        assert!(!looks_like_auth_failure(""));
    }

    #[test]
    fn error_display_matches_the_failure_classifier_markers() {
        // These substrings are what `channel_reply::classify_cli_failure`
        // routes on — if this test fails, failover stops working.
        let auth = GenericCliError::AuthRequired {
            runtime: "kimi",
            action: "a".into(),
            stderr_tail: "b".into(),
        }
        .to_string()
        .to_lowercase();
        assert!(auth.contains("not logged in"), "{auth}");

        let to = GenericCliError::Timeout { runtime: "kimi", secs: 300 }
            .to_string()
            .to_lowercase();
        assert!(to.contains("hard timeout"), "{to}");

        let empty = GenericCliError::EmptyOutput { runtime: "kimi", stderr_tail: String::new() }
            .to_string()
            .to_lowercase();
        assert!(empty.contains("empty response"), "{empty}");

        let spawn = GenericCliError::Spawn { runtime: "kimi", error: "x".into() }
            .to_string()
            .to_lowercase();
        assert!(spawn.contains("spawn error"), "{spawn}");
    }

    #[test]
    fn auth_action_names_the_real_login_command_and_env_var() {
        let a = auth_action(spec_for("kimi").unwrap());
        assert!(a.contains("kimi login"), "{a}");
        let q = auth_action(spec_for("qwen").unwrap());
        assert!(
            q.contains("DASHSCOPE_API_KEY"),
            "qwen has no login subcommand — the action must name the key: {q}"
        );
    }

    // ── fake-binary integration ─────────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn text_output_runtime_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        // Copilot's `-s` mode: the answer is plain stdout, nothing else.
        let bin = fake_bin(dir.path(), "copilot", "echo 'the plain answer'");
        let rt = GenericCliRuntime::with_program(spec_for("copilot").unwrap(), bin.to_string_lossy());
        assert!(rt.is_available().await);
        let resp = rt.execute("hi", &ctx(dir.path())).await.unwrap();
        assert_eq!(resp.content, "the plain answer");
        assert_eq!(resp.runtime_name, "copilot");
        assert!(resp.input_tokens > 0, "tokens are estimated, not zero");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn json_output_runtime_extracts_the_terminal_result() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_bin(
            dir.path(),
            "qwen",
            r#"echo '[{"type":"assistant","message":{"content":[{"text":"draft"}]}},{"type":"result","subtype":"success","result":"json answer"}]'"#,
        );
        let rt = GenericCliRuntime::with_program(spec_for("qwen").unwrap(), bin.to_string_lossy());
        let resp = rt.execute("hi", &ctx(dir.path())).await.unwrap();
        assert_eq!(resp.content, "json answer");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn jsonl_output_runtime_folds_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_bin(
            dir.path(),
            "kimi",
            concat!(
                r#"echo '{"role":"assistant","content":"first chunk"}'"#,
                "\n",
                r#"echo '{"role":"tool","tool_call_id":"t","content":"ignored"}'"#,
                "\n",
                r#"echo '{"role":"assistant","content":"jsonl answer"}'"#,
            ),
        );
        let rt = GenericCliRuntime::with_program(spec_for("kimi").unwrap(), bin.to_string_lossy());
        let resp = rt.execute("hi", &ctx(dir.path())).await.unwrap();
        assert_eq!(resp.content, "jsonl answer");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auth_required_failure_is_typed_and_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_bin(
            dir.path(),
            "kimi",
            "echo 'Error: not logged in. Run `kimi login`.' >&2\nexit 1",
        );
        let rt = GenericCliRuntime::with_program(spec_for("kimi").unwrap(), bin.to_string_lossy());
        let err = rt.run("hi", &ctx(dir.path())).await.unwrap_err();
        assert!(
            matches!(err, GenericCliError::AuthRequired { runtime: "kimi", .. }),
            "{err:?}"
        );
        let msg = err.to_string();
        assert!(msg.to_lowercase().contains("not logged in"), "{msg}");
        assert!(msg.contains("kimi login"), "must name the fix: {msg}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_zero_exit_without_auth_markers_is_a_plain_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_bin(dir.path(), "copilot", "echo 'boom: disk full' >&2\nexit 3");
        let rt = GenericCliRuntime::with_program(spec_for("copilot").unwrap(), bin.to_string_lossy());
        let err = rt.run("hi", &ctx(dir.path())).await.unwrap_err();
        assert!(
            matches!(err, GenericCliError::NonZeroExit { code: 3, .. }),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_zero_with_no_output_is_a_failure_not_an_empty_answer() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_bin(dir.path(), "copilot", "exit 0");
        let rt = GenericCliRuntime::with_program(spec_for("copilot").unwrap(), bin.to_string_lossy());
        let err = rt.run("hi", &ctx(dir.path())).await.unwrap_err();
        assert!(matches!(err, GenericCliError::EmptyOutput { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_delivered_prompt_reaches_the_child() {
        let dir = tempfile::tempdir().unwrap();
        // `opencode run` takes the prompt as an argument in the catalog; use a
        // synthetic stdin spec to prove the stdin branch works end to end.
        static STDIN_SPEC: RuntimeSpec = RuntimeSpec {
            id: "opencode",
            display_name: "stdin fixture",
            binary: "cat",
            aliases: &[],
            binary_aliases: &[],
            install: duduclaw_core::runtime_catalog::InstallChannel::Manual {
                command: "n/a",
                reason: "test_fixture",
            },
            headless: duduclaw_core::runtime_catalog::HeadlessSpec {
                args_template: &["-"],
                output: OutputFormat::Text,
                model_flag: None,
                model_env: None,
                extra_env: &[],
            },
            auth: duduclaw_core::runtime_catalog::AuthSpec {
                api_key_env: None,
                login: duduclaw_core::runtime_catalog::LoginMethod::None,
                credential_paths: &[],
                tos_note: None,
                login_hint: None,
            },
            mcp: false,
            model_prefixes: &[],
            fallback_models: &[],
            vendor_url: "https://example.invalid",
            verified: false,
        };
        assert!(STDIN_SPEC.headless.prompt_via_stdin());
        let rt = GenericCliRuntime::with_program(&STDIN_SPEC, "/bin/cat");
        let resp = rt.execute("piped question", &ctx(dir.path())).await.unwrap();
        assert!(
            resp.content.contains("piped question"),
            "cat echoes the payload it received on stdin: {}",
            resp.content
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_binary_is_a_spawn_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let rt = GenericCliRuntime::with_program(
            spec_for("copilot").unwrap(),
            dir.path().join("nope").to_string_lossy(),
        );
        assert!(!rt.is_available().await);
        let err = rt.run("hi", &ctx(dir.path())).await.unwrap_err();
        assert!(matches!(err, GenericCliError::Spawn { .. }), "{err:?}");
    }

    #[test]
    fn detect_finds_a_binary_on_a_temp_path() {
        // Detection through the catalog: a fake `copilot` in a HOME-rooted
        // candidate dir must be found by `detect_runtime`.
        let home = tempfile::tempdir().unwrap();
        let bindir = home.path().join(".local/bin");
        std::fs::create_dir_all(&bindir).unwrap();
        fake_bin(&bindir, "copilot", "true");
        let found = duduclaw_core::which_runtime_in_home(home.path(), "copilot");
        assert!(found.is_some(), "catalog-driven probe must find ~/.local/bin/copilot");
        assert!(found.unwrap().ends_with("/copilot"));
        // …and an id that isn't in the catalog resolves to nothing at all.
        assert!(duduclaw_core::which_runtime_in_home(home.path(), "not-a-runtime").is_none());
    }
}
