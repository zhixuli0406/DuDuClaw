//! OpenAI Codex CLI runtime — `codex exec --json` JSONL streaming.
//!
//! Codex CLI outputs JSONL events on stdout when invoked with `--json`:
//!   - `thread.started` — session created
//!   - `turn.started` / `turn.completed` — contains token usage
//!   - `item.completed` (type=message) — assistant text content
//!
//! Authentication: `OPENAI_API_KEY` environment variable.

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{info, warn};

use duduclaw_core::types::{sandbox_level_for, CapabilitiesConfig};

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Derive Codex CLI sandbox/approval flags from the agent's capabilities.
///
/// Replaces the former blanket `--full-auto` (which unconditionally implied
/// `workspace-write` + no approvals, ignoring `CapabilitiesConfig` entirely).
/// Non-interactive `codex exec` requires an approval policy, so we always pass
/// `--ask-for-approval never` and scope the blast radius via `--sandbox`:
/// - restrictive caps (no write tools, no browser/computer use) → `read-only`
/// - default / `None` caps → `workspace-write` (same write scope `--full-auto` granted)
/// - explicit `computer_use = true` grant → `danger-full-access`
fn sandbox_args(caps: Option<&CapabilitiesConfig>) -> Vec<String> {
    let level = sandbox_level_for(caps);
    vec![
        "--ask-for-approval".to_string(),
        "never".to_string(),
        "--sandbox".to_string(),
        level.as_codex_flag().to_string(),
    ]
}

/// `-c` config-override args registering the duduclaw MCP server for THIS
/// invocation. Codex only reads MCP servers from `$CODEX_HOME/config.toml`;
/// redirecting `CODEX_HOME` at the agent dir would orphan the user's
/// `~/.codex/auth.json` (breaking ChatGPT-plan OAuth), so per-invocation
/// `--config` overrides are the safe way to guarantee registration.
fn mcp_override_args(agent_id: &str) -> Vec<String> {
    let Some(def) = super::duduclaw_mcp_server_json(agent_id) else {
        return Vec::new();
    };
    let mut args = Vec::new();
    if let Some(command) = def.get("command").and_then(|c| c.as_str()) {
        args.push("-c".to_string());
        args.push(format!("mcp_servers.duduclaw.command={command}"));
    }
    args.push("-c".to_string());
    args.push(r#"mcp_servers.duduclaw.args=["mcp-server"]"#.to_string());
    if let Some(env) = def.get("env").and_then(|e| e.as_object()) {
        for (k, v) in env {
            if let Some(val) = v.as_str() {
                args.push("-c".to_string());
                args.push(format!("mcp_servers.duduclaw.env.{k}={val}"));
            }
        }
    }
    args
}

/// Runtime that delegates to the OpenAI Codex CLI.
pub struct CodexRuntime {
    codex_path: String,
}

impl CodexRuntime {
    pub fn new() -> Self {
        Self {
            codex_path: "codex".to_string(),
        }
    }
}

// ── JSONL event types ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// Parse a complete `codex exec --json` stdout buffer into `(content,
/// input_tokens, output_tokens, chunks)`.
///
/// T10 (design commercial/docs/design-task-forward-model-2026-08-06.md
/// §8.2/§9): the previously zero-constructor `RuntimeChunk::ToolUse` /
/// `ToolResult` variants (`runtime/mod.rs`) are populated from `item.
/// completed` events whose `item.type` is `command_execution` or
/// `mcp_tool_call` — the two tool-item types `codex exec --json` emits
/// alongside the `message` item this parser already reads for `content`.
/// `execute()` below folds `chunks` into `NativeToolEvent`s via
/// [`super::native_tool_events_from_chunks`] — the same runtime-neutral fold
/// `gemini.rs` uses, so `prediction::task_observe` never needs
/// codex-specific code (design §8.2's stated purpose for routing through
/// `RuntimeChunk`).
///
/// Schema grounded in the published `codex exec --json` event reference
/// (item fields: `status` ∈ {in_progress, completed, failed};
/// `command_execution` carries `command`/`aggregated_output`/`exit_code`;
/// `mcp_tool_call` carries `server`/`tool`/`arguments`/`result`/`error`) —
/// NOT verified against the Rust source (`codex-rs/core/protocol.rs`)
/// directly, so exact field names are treated as best-effort: an
/// absent/renamed field degrades to skipping that event, never a
/// fabricated tool name or success value.
fn parse_codex_stdout(stdout: &str) -> (String, u64, u64, Vec<super::RuntimeChunk>) {
    use super::RuntimeChunk;

    let mut content = String::new();
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut chunks: Vec<RuntimeChunk> = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<CodexEvent>(line) {
            match event.event_type.as_str() {
                "item.completed" => {
                    // Extract text from message items
                    if let Some(item) = event.extra.get("item") {
                        match item.get("type").and_then(|t| t.as_str()) {
                            Some("message") => {
                                if let Some(text) = item
                                    .get("content")
                                    .and_then(|c| c.as_array())
                                    .and_then(|arr| arr.iter().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("output_text")))
                                    .and_then(|b| b.get("text"))
                                    .and_then(|t| t.as_str())
                                {
                                    content = text.to_string();
                                }
                            }
                            Some("command_execution") => {
                                // ToolClass::classify maps codex's shell tool
                                // via the literal name "shell" (see
                                // `prediction/tool_class.rs`'s cross-runtime
                                // Exec alias table) — this is a synthesized
                                // label, not something read off the wire
                                // (the command_execution item has no
                                // separate "tool name" field; the item TYPE
                                // itself is the signal).
                                let status = item.get("status").and_then(|s| s.as_str());
                                // "failed" is the only documented failure
                                // status; anything else (including an
                                // absent field) is treated as
                                // attempted-and-not-known-to-have-failed,
                                // matching this collector's optimistic
                                // default on the claude/gemini paths.
                                let is_error = status == Some("failed");
                                let input = item.get("command").cloned().unwrap_or(serde_json::Value::Null);
                                // R1: the documented `aggregated_output` field
                                // carries the shell command's actual stdout —
                                // captured verbatim here; masking + capping
                                // happens downstream in
                                // `native_tool_events_from_chunks` (never
                                // guessed at when absent).
                                let output = item
                                    .get("aggregated_output")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                chunks.push(RuntimeChunk::ToolUse { name: "shell".to_string(), input });
                                chunks.push(RuntimeChunk::ToolResult { output, is_error });
                            }
                            Some("mcp_tool_call") => {
                                if let Some(tool) = item.get("tool").and_then(|t| t.as_str()) {
                                    let status = item.get("status").and_then(|s| s.as_str());
                                    let is_error = status == Some("failed");
                                    let input = item.get("arguments").cloned().unwrap_or(serde_json::Value::Null);
                                    // R1: dual-name tolerance — the documented
                                    // shape is `result.content[].text`
                                    // (mirrors an MCP tool_result content
                                    // array); on failure fall back to
                                    // `error.message`. Neither present ⇒
                                    // empty output, never fabricated.
                                    let output = extract_mcp_tool_call_output(&item)
                                        .unwrap_or_default();
                                    chunks.push(RuntimeChunk::ToolUse { name: tool.to_string(), input });
                                    chunks.push(RuntimeChunk::ToolResult { output, is_error });
                                }
                                // No "tool" field ⇒ skip (don't fabricate a
                                // name) rather than record an "unknown"
                                // placeholder.
                            }
                            _ => {}
                        }
                    }
                }
                "turn.completed" => {
                    // Extract token usage
                    if let Some(usage) = event.extra.get("usage") {
                        if let Ok(u) = serde_json::from_value::<CodexUsage>(usage.clone()) {
                            input_tokens = u.input_tokens;
                            output_tokens = u.output_tokens;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    (content, input_tokens, output_tokens, chunks)
}

/// R1: extract an `mcp_tool_call` item's output text for `RuntimeChunk::ToolResult`.
/// Tries the documented success shape first (`result.content[].text`, the
/// same content-block array MCP `tool_result`s use), then falls back to
/// `error.message` on failure. `None` when neither is present — the caller
/// treats that as "nothing captured", never fabricates a placeholder.
fn extract_mcp_tool_call_output(item: &serde_json::Value) -> Option<String> {
    if let Some(arr) = item.pointer("/result/content").and_then(|c| c.as_array()) {
        let parts: Vec<&str> = arr
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect();
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    // A bare string result (undocumented but tolerated — dual-shape).
    if let Some(s) = item.get("result").and_then(|r| r.as_str()) {
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    item.pointer("/error/message")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for CodexRuntime {
    fn name(&self) -> &str {
        "codex"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        info!(agent = %context.agent_id, "CodexRuntime: executing via codex exec --json");

        // Limit system_prompt to 64KB to avoid ARG_MAX issues.
        // Char-boundary-safe truncation (never raw byte-index slicing on
        // potentially CJK/emoji content — 2026-06 review convention #1).
        const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;
        let system_prompt: &str = if context.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            tracing::warn!(
                agent = %context.agent_id,
                original_len = context.system_prompt.len(),
                "system_prompt truncated to 64KB"
            );
            duduclaw_core::truncate_bytes(&context.system_prompt, MAX_SYSTEM_PROMPT_BYTES)
        } else {
            &context.system_prompt
        };

        // Prevent argument injection: prompts starting with '-' would be parsed as flags
        let safe_prompt = if prompt.starts_with('-') {
            format!(" {prompt}")
        } else {
            prompt.to_string()
        };

        // W1 (capability enforcement): derive sandbox/approval flags from the
        // agent's capabilities instead of the former blanket `--full-auto`.
        let caps = context.capabilities.as_ref();
        let level = sandbox_level_for(caps);
        if let Some(c) = caps {
            if c.has_tool_restrictions() {
                warn!(
                    runtime = "codex",
                    agent = %context.agent_id,
                    sandbox = level.as_codex_flag(),
                    "capability enforcement is best-effort on this runtime — \
                     per-tool allow/deny lists collapse to a coarse --sandbox level"
                );
            }
        }

        // W2 (MCP wiring): register the duduclaw MCP server before spawning.
        // 1) Per-invocation `-c` overrides — effective regardless of CODEX_HOME.
        // 2) Best-effort per-agent `.codex/config.toml` for operators who run
        //    codex manually in the agent dir with CODEX_HOME pointed there.
        //    Warn-not-fatal: MCP registration failing must not block the reply.
        if let Some(ref dir) = context.agent_dir {
            if let Err(e) = Self::ensure_duduclaw_mcp_config(dir, &context.agent_id) {
                warn!(
                    runtime = "codex",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write per-agent codex MCP config — continuing without it"
                );
            }
        }

        let mut cmd = tokio::process::Command::new(&self.codex_path);
        cmd.arg("exec").arg("--json");
        cmd.args(sandbox_args(caps));
        cmd.args(mcp_override_args(&context.agent_id));

        // Pass system prompt via AGENTS.md in working directory.
        // Codex exec has no --instructions flag; it reads from AGENTS.md.
        if !system_prompt.is_empty() {
            if let Some(ref dir) = context.agent_dir {
                let agents_md = dir.join("AGENTS.md");
                let _ = std::fs::write(&agents_md, system_prompt);
            }
        }

        // Prepend conversation history to prompt (Codex exec has no native multi-turn)
        let augmented_prompt = if context.conversation_history.is_empty() {
            safe_prompt
        } else {
            super::format_history_as_prompt(&context.conversation_history, &safe_prompt)
        };

        cmd.arg(&augmented_prompt);

        // Set model if specified
        if !context.model.is_empty() {
            cmd.arg("-m").arg(&context.model);
        }

        // Set working directory
        if let Some(ref dir) = context.agent_dir {
            cmd.arg("--cd").arg(dir);
        }

        // Pass API key if available
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("OPENAI_API_KEY", &api_key);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). Layered on top of the CLI `--sandbox`
        // flag; fail-closed if required but unavailable.
        super::apply_native_sandbox(&mut cmd, caps, context.agent_dir.as_deref(), "codex")?;

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Codex CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn codex: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Codex CLI exited with {}: {}", output.status, stderr.chars().take(500).collect::<String>()));
        }

        // Parse JSONL output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (mut content, input_tokens, output_tokens, chunks) = parse_codex_stdout(&stdout);
        super::extend_native_tool_events(super::native_tool_events_from_chunks(&chunks));

        if content.is_empty() {
            // Fallback: use the last line as content
            content = stdout.lines().last().unwrap_or("").to_string();
        }

        // Still empty ⇒ FAILURE, not success: Ok("") would be silently dropped
        // by every channel and poison the session with an empty assistant turn.
        if content.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Empty response from Codex CLI (exit 0); stderr tail: {}",
                duduclaw_core::truncate_bytes(stderr.trim(), 300)
            ));
        }

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "codex".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.codex_path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// ── Streaming ───────────────────────────────────────────────────

impl CodexRuntime {
    /// Execute and return chunks. Codex CLI does not support true streaming,
    /// so this wraps the normal execution into a single `Done` chunk.
    pub async fn execute_streaming(
        &self,
        prompt: &str,
        context: &super::RuntimeContext,
    ) -> Result<Vec<super::RuntimeChunk>, String> {
        let response = self.execute(prompt, context).await?;
        Ok(vec![super::RuntimeChunk::Done(response)])
    }
}

// ── MCP config ──────────────────────────────────────────────────

impl CodexRuntime {
    /// Render `[mcp_servers]` TOML deterministically (sorted server names and
    /// keys — a `HashMap` iteration order would make the idempotence check in
    /// [`Self::write_mcp_config`] flap between runs).
    fn render_mcp_toml(servers: &std::collections::HashMap<String, serde_json::Value>) -> String {
        fn toml_string(s: &str) -> String {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
        let mut content = String::from("[mcp_servers]\n");
        let mut names: Vec<&String> = servers.keys().collect();
        names.sort();
        for name in names {
            let config = &servers[name];
            if name.contains('.') {
                content.push_str(&format!("[mcp_servers.{}]\n", toml_string(name)));
            } else {
                content.push_str(&format!("[mcp_servers.{name}]\n"));
            }
            let Some(obj) = config.as_object() else { continue };
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for k in keys {
                let v = &obj[k.as_str()];
                let toml_val = match v {
                    serde_json::Value::String(s) => format!("{k} = {}\n", toml_string(s)),
                    serde_json::Value::Array(arr) => {
                        let items: Vec<String> = arr
                            .iter()
                            .map(|item| {
                                if let Some(s) = item.as_str() {
                                    toml_string(s)
                                } else {
                                    item.to_string()
                                }
                            })
                            .collect();
                        format!("{k} = [{}]\n", items.join(", "))
                    }
                    serde_json::Value::Object(env) => {
                        // Inline table (used for the `env` map).
                        let mut env_keys: Vec<&String> = env.keys().collect();
                        env_keys.sort();
                        let pairs: Vec<String> = env_keys
                            .iter()
                            .filter_map(|ek| {
                                env[ek.as_str()]
                                    .as_str()
                                    .map(|ev| format!("{ek} = {}", toml_string(ev)))
                            })
                            .collect();
                        format!("{k} = {{ {} }}\n", pairs.join(", "))
                    }
                    _ => format!("{k} = {v}\n"),
                };
                content.push_str(&toml_val);
            }
        }
        content
    }

    /// Write MCP server configuration to the agent's codex config
    /// (`<agent_dir>/.codex/config.toml`). Idempotent: skips the write when the
    /// file already holds exactly the desired content. Returns `Ok(true)` when
    /// written, `Ok(false)` when already up to date.
    pub fn write_mcp_config(
        agent_dir: &std::path::Path,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let config_path = agent_dir.join(".codex").join("config.toml");
        let content = Self::render_mcp_toml(servers);
        if let Ok(existing) = std::fs::read_to_string(&config_path) {
            if existing == content {
                return Ok(false);
            }
        }
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&config_path, content).map_err(|e| e.to_string())?;
        // Carries DUDUCLAW_AGENT_TOKEN in plaintext — restrict to the owning
        // OS user (0600 on Unix; no-op on Windows).
        duduclaw_core::platform::set_owner_only(&config_path).ok();
        Ok(true)
    }

    /// W2: ensure the duduclaw MCP server (absolute binary + `mcp-server` arg +
    /// `DUDUCLAW_AGENT_ID` env) is registered in the agent's codex config.
    /// Called from [`AgentRuntime::execute`] before every spawn — cheap
    /// check-before-write keeps it idempotent.
    pub fn ensure_duduclaw_mcp_config(
        agent_dir: &std::path::Path,
        agent_id: &str,
    ) -> Result<bool, String> {
        let Some(def) = super::duduclaw_mcp_server_json(agent_id) else {
            return Err("duduclaw binary did not resolve to an absolute path".to_string());
        };
        let mut servers = std::collections::HashMap::new();
        servers.insert("duduclaw".to_string(), def);
        Self::write_mcp_config(agent_dir, &servers)
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_codex_event() {
        let line = r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":50}}"#;
        let event: CodexEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "turn.completed");
        let usage: CodexUsage = serde_json::from_value(event.extra.get("usage").unwrap().clone()).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
    }

    // ── T10: parse_codex_stdout → RuntimeChunk → NativeToolEvent ────────

    #[test]
    fn parse_codex_stdout_emits_tool_use_and_tool_result_chunks() {
        // The literal T10 ask: `RuntimeChunk::ToolUse`/`ToolResult` must
        // actually get constructed, not bypassed.
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsrc\n","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        assert_eq!(chunks.len(), 2);
        assert!(matches!(chunks[0], super::super::RuntimeChunk::ToolUse { .. }));
        match &chunks[1] {
            super::super::RuntimeChunk::ToolResult { is_error, .. } => assert!(!is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parse_codex_stdout_collects_command_execution_success() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsrc\n","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "shell");
        assert!(events[0].success);
    }

    #[test]
    fn parse_codex_stdout_collects_command_execution_failure() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc false","aggregated_output":"","exit_code":1,"status":"failed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "shell");
        assert!(!events[0].success);
    }

    #[test]
    fn parse_codex_stdout_collects_mcp_tool_call() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_5","type":"mcp_tool_call","server":"duduclaw","tool":"tasks_create","arguments":{"title":"x"},"result":{"content":[{"type":"text","text":"ok"}]},"error":null,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "tasks_create");
        assert!(events[0].success);
        assert_eq!(events[0].result_text.as_deref(), Some("ok"));
        assert!(events[0].input_text.as_deref().unwrap().contains("title"));
    }

    #[test]
    fn parse_codex_stdout_collects_mcp_tool_call_failure() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_6","type":"mcp_tool_call","server":"duduclaw","tool":"tasks_create","arguments":null,"result":null,"error":{"message":"boom"},"status":"failed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert!(!events[0].success);
        // R1: on failure, the item's `error.message` is captured as
        // `result_text` — dual-shape fallback from the documented
        // `result.content[].text` success shape.
        assert_eq!(events[0].result_text.as_deref(), Some("boom"));
    }

    // ── R1: result_text / input_text capture ─────────────────────────────

    #[test]
    fn parse_codex_stdout_command_execution_captures_aggregated_output() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc ls","aggregated_output":"docs\nsrc\n","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events[0].result_text.as_deref(), Some("docs\nsrc"));
        assert!(events[0].input_text.as_deref().unwrap().contains("bash -lc ls"));
    }

    #[test]
    fn parse_codex_stdout_command_execution_empty_output_is_none() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"bash -lc true","aggregated_output":"","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert!(events[0].result_text.is_none());
    }

    #[test]
    fn parse_codex_stdout_masks_secret_in_command_execution_output() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"cat .env","aggregated_output":"ANTHROPIC_API_KEY=sk-ant-api03-verysecretvalue1234567890","exit_code":0,"status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        let result_text = events[0].result_text.as_deref().unwrap();
        assert!(
            !result_text.contains("sk-ant-api03-verysecretvalue1234567890"),
            "secret leaked into result_text: {result_text}"
        );
    }

    #[test]
    fn parse_codex_stdout_mixed_events_content_and_tools() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"ls","aggregated_output":"x","exit_code":0,"status":"completed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","server":"duduclaw","tool":"memory_search","arguments":{},"result":{},"error":null,"status":"completed"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"Hello world"}]}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}"#,
            "\n",
        );
        let (content, input_tokens, output_tokens, chunks) = parse_codex_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(content, "Hello world");
        assert_eq!(input_tokens, 10);
        assert_eq!(output_tokens, 5);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].tool_name, "shell");
        assert_eq!(events[1].tool_name, "memory_search");
    }

    #[test]
    fn parse_codex_stdout_missing_tool_field_skips_not_fabricates() {
        // mcp_tool_call with no "tool" field — must be skipped, never
        // recorded under a fabricated "unknown" placeholder.
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"id":"item_7","type":"mcp_tool_call","server":"duduclaw","status":"completed"}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_codex_stdout(stdout);
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_codex_stdout_no_tool_items_is_empty_events() {
        let stdout = concat!(
            r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"hi"}]}}"#,
            "\n",
        );
        let (content, _, _, chunks) = parse_codex_stdout(stdout);
        assert_eq!(content, "hi");
        assert!(chunks.is_empty());
    }

    fn caps(
        computer_use: bool,
        browser_via_bash: bool,
        allowed: &[&str],
        denied: &[&str],
    ) -> CapabilitiesConfig {
        CapabilitiesConfig {
            computer_use,
            browser_via_bash,
            allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
            denied_tools: denied.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn sandbox_args_default_caps_is_workspace_write() {
        // Default caps (empty allowlist ⇒ full default toolset incl. Bash/Write)
        // keep the write scope --full-auto used to grant — workspace-write.
        let c = caps(false, false, &[], &[]);
        let args = sandbox_args(Some(&c));
        assert_eq!(
            args,
            vec!["--ask-for-approval", "never", "--sandbox", "workspace-write"]
        );
    }

    #[test]
    fn sandbox_args_none_caps_keeps_legacy_workspace_write() {
        let args = sandbox_args(None);
        assert_eq!(
            args,
            vec!["--ask-for-approval", "never", "--sandbox", "workspace-write"]
        );
    }

    #[test]
    fn sandbox_args_read_only_when_allowlist_has_no_write_tools() {
        let c = caps(false, false, &["Read", "Grep", "WebSearch"], &[]);
        let args = sandbox_args(Some(&c));
        assert_eq!(args[3], "read-only");
    }

    #[test]
    fn sandbox_args_read_only_when_all_write_tools_denied() {
        let c = caps(
            false,
            false,
            &[],
            &["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"],
        );
        assert_eq!(sandbox_args(Some(&c))[3], "read-only");
    }

    #[test]
    fn sandbox_args_full_access_only_on_explicit_computer_use() {
        let c = caps(true, false, &[], &[]);
        assert_eq!(sandbox_args(Some(&c))[3], "danger-full-access");
    }

    #[test]
    fn sandbox_args_browser_via_bash_forces_workspace_write() {
        // A read-only allowlist + browser_via_bash still needs bash → not read-only.
        let c = caps(false, true, &["Read"], &[]);
        assert_eq!(sandbox_args(Some(&c))[3], "workspace-write");
    }

    #[test]
    fn sandbox_args_qualified_bash_allow_counts_as_write() {
        // `Bash(git:*)` is an anchored token grant of (scoped) Bash — must not
        // collapse to read-only, but must never escalate past workspace-write.
        let c = caps(false, false, &["Read", "Bash(git:*)"], &[]);
        assert_eq!(sandbox_args(Some(&c))[3], "workspace-write");
    }

    #[test]
    fn mcp_config_write_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "duduclaw".to_string(),
            serde_json::json!({
                "command": "/usr/local/bin/duduclaw",
                "args": ["mcp-server"],
                "env": { "DUDUCLAW_AGENT_ID": "agnes" },
            }),
        );
        assert!(CodexRuntime::write_mcp_config(dir.path(), &servers).unwrap());
        // Second call: identical content → no write reported.
        assert!(!CodexRuntime::write_mcp_config(dir.path(), &servers).unwrap());

        let content =
            std::fs::read_to_string(dir.path().join(".codex").join("config.toml")).unwrap();
        assert!(content.contains("[mcp_servers.duduclaw]"));
        assert!(content.contains("command = \"/usr/local/bin/duduclaw\""));
        assert!(content.contains("args = [\"mcp-server\"]"));
        assert!(content.contains("DUDUCLAW_AGENT_ID = \"agnes\""));
    }

    #[test]
    fn mcp_override_args_carry_agent_id_env() {
        // resolve_duduclaw_bin falls back to current_exe (absolute in tests),
        // so overrides should materialize with the agent-id env override.
        let args = mcp_override_args("agnes");
        if args.is_empty() {
            return; // binary not resolvable to an absolute path in this env
        }
        assert!(args.iter().any(|a| a == r#"mcp_servers.duduclaw.args=["mcp-server"]"#));
        assert!(
            args.iter()
                .any(|a| a == "mcp_servers.duduclaw.env.DUDUCLAW_AGENT_ID=agnes"),
            "agent id env override missing: {args:?}"
        );
    }

    #[test]
    fn test_parse_item_completed() {
        let line = r#"{"type":"item.completed","item":{"type":"message","content":[{"type":"output_text","text":"Hello world"}]}}"#;
        let event: CodexEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "item.completed");
        let text = event.extra
            .get("item").unwrap()
            .get("content").unwrap()
            .as_array().unwrap()[0]
            .get("text").unwrap()
            .as_str().unwrap();
        assert_eq!(text, "Hello world");
    }
}
