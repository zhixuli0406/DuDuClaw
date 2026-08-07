//! Google Gemini CLI runtime — `gemini -p --output-format stream-json`.
//!
//! Gemini CLI outputs JSONL events on stdout:
//!   - `init` — session metadata
//!   - `message` (role=assistant) — text content
//!   - `tool_use` / `tool_result` — tool interactions
//!   - `result` — final outcome with aggregated stats
//!
//! Authentication: `GEMINI_API_KEY` environment variable or Google OAuth.
//! Exit codes: 0=success, 1=error, 42=input error, 53=turn limit.

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{info, warn};

use duduclaw_core::types::{sandbox_level_for, CapabilitiesConfig, SandboxLevel};

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Derive Gemini CLI approval/sandbox flags from the agent's capabilities.
///
/// Replaces the former blanket `--approval-mode yolo` (auto-approve every
/// tool call regardless of `CapabilitiesConfig` — a deny-by-default
/// violation). A headless subprocess still needs a non-interactive approval
/// policy, so:
/// - explicit `computer_use = true` grant → `yolo` (the operator opted in)
/// - default / `None` caps → `auto_edit` (edit tools auto-approved; other
///   privileged tools are refused instead of silently approved — fail closed)
/// - restrictive caps (no write tools, no browser use) → `auto_edit` +
///   `--sandbox` so tool execution is container-jailed
fn approval_args(caps: Option<&CapabilitiesConfig>) -> Vec<String> {
    match sandbox_level_for(caps) {
        SandboxLevel::FullAccess => {
            vec!["--approval-mode".to_string(), "yolo".to_string()]
        }
        SandboxLevel::WorkspaceWrite => {
            vec!["--approval-mode".to_string(), "auto_edit".to_string()]
        }
        SandboxLevel::ReadOnly => vec![
            "--approval-mode".to_string(),
            "auto_edit".to_string(),
            "--sandbox".to_string(),
        ],
    }
}

/// Runtime that delegates to the Google Gemini CLI.
pub struct GeminiRuntime {
    gemini_path: String,
}

impl GeminiRuntime {
    pub fn new() -> Self {
        Self {
            gemini_path: "gemini".to_string(),
        }
    }
}

// ── JSONL event types ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GeminiEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiStats {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

/// Parse a complete `gemini -p --output-format stream-json` stdout buffer
/// into `(content, input_tokens, output_tokens, chunks)`.
///
/// T10 (design commercial/docs/design-task-forward-model-2026-08-06.md
/// §8.2/§9): the previously zero-constructor `RuntimeChunk::ToolUse` /
/// `ToolResult` variants (`runtime/mod.rs`) are populated from the
/// `tool_use` / `tool_result` event types this module's own header doc
/// comment already documented as part of the stream (design §1 line 37
/// flagged this: "gemini 模組註解甚至明列了 tool_use/tool_result 事件存
/// 在"). Pairing (below, before the `RuntimeChunk` conversion) mirrors
/// `claude_runner.rs`'s WP-A4 approach: match by an id field when present,
/// else fall back to the most recently opened call — unlike codex's
/// `command_execution`/`mcp_tool_call` (always emitted as one adjacent
/// completed item), gemini's `tool_use`/`tool_result` are independent events
/// that CAN arrive out of order across multiple outstanding calls, so this
/// id-based resolution happens on the raw event stream FIRST; only the
/// already-resolved `(name, success)` pairs are then emitted as adjacent
/// `ToolUse`+`ToolResult` chunk pairs (in resolution order) —
/// [`super::native_tool_events_from_chunks`]'s simple "next chunk pairs with
/// this one" fold only needs to handle that already-normalized shape, not
/// gemini's actual out-of-order wire behavior.
///
/// Exact field names (`tool_name`/`name`, `tool_id`/`id`, `status`) are
/// grounded in a third-party `stream-json` event cheat sheet (no first-party
/// field-level schema doc was found) — kept deliberately tolerant (checks
/// both plausible field names) rather than committing to one unverified
/// name; a call with neither field present degrades to tool_name
/// `"unknown"` (still recorded — `ToolClass::classify` maps unknown names to
/// `Other` rather than the caller silently losing the call count) instead
/// of being dropped, since unlike codex's mcp_tool_call (which does have a
/// clearly-documented `tool` field) there is no unambiguous "skip" signal
/// here.
///
/// R1: `tool_use.parameters`/`input`/`args` (dual/triple-name tolerance,
/// same convention as the name/id fields above) and
/// `tool_result.output`/`content` are now carried through into the emitted
/// `RuntimeChunk::ToolUse.input`/`ToolResult.output` — previously these were
/// always `Value::Null`/`String::new()`, discarding whatever text the wire
/// event actually had. Resolution (pairing a `tool_use` with its eventual
/// `tool_result`, id-based, out-of-order tolerant) still happens on the raw
/// event stream first, exactly as before; only the *payload* carried through
/// the resolved tuple changed.
fn parse_gemini_stdout(stdout: &str) -> (String, u64, u64, Vec<super::RuntimeChunk>) {
    use super::RuntimeChunk;

    let mut content = String::new();
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    // Each resolved tool call: (name, input, output, is_error). Provisional
    // until a matching `tool_result` (if any) arrives.
    let mut resolved: Vec<(String, serde_json::Value, String, bool)> = Vec::new();
    // Outstanding (unresolved) tool calls, innermost last: (id-or-empty,
    // index into `resolved`). Mirrors the WP-A4 claude_runner.rs pairing.
    let mut open_native_calls: Vec<(String, usize)> = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<GeminiEvent>(line) {
            match event.event_type.as_str() {
                "message" => {
                    // Extract assistant message text
                    if event.extra.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                        if let Some(text) = event.extra.get("content").and_then(|c| c.as_str()) {
                            content.push_str(text);
                        }
                    }
                }
                "result" => {
                    // Final result: extract content and stats.
                    // Only overwrite if non-empty — when the AI uses tools, the
                    // result event may have empty content while the real answer
                    // was accumulated from earlier "message" events.
                    if let Some(text) = event.extra.get("content").and_then(|c| c.as_str()) {
                        if !text.is_empty() {
                            content = text.to_string();
                        }
                    }
                    if let Some(stats) = event.extra.get("stats") {
                        if let Ok(s) = serde_json::from_value::<GeminiStats>(stats.clone()) {
                            input_tokens = s.input_tokens;
                            output_tokens = s.output_tokens;
                        }
                    }
                }
                "tool_use" => {
                    let name = event
                        .extra
                        .get("tool_name")
                        .or_else(|| event.extra.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let id = event
                        .extra
                        .get("tool_id")
                        .or_else(|| event.extra.get("id"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_default();
                    // R1: `parameters` is the field the cheat sheet documents
                    // for this event; `input`/`args` tolerated as plausible
                    // alternates (same tolerance style as name/id above).
                    let input = event
                        .extra
                        .get("parameters")
                        .or_else(|| event.extra.get("input"))
                        .or_else(|| event.extra.get("args"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let idx = resolved.len();
                    open_native_calls.push((id, idx));
                    // Provisional success — corrected by a paired
                    // `tool_result` below, if one arrives.
                    resolved.push((name, input, String::new(), false));
                }
                "tool_result" => {
                    let id = event
                        .extra
                        .get("tool_id")
                        .or_else(|| event.extra.get("id"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let is_error = event
                        .extra
                        .get("status")
                        .and_then(|s| s.as_str())
                        .map(|s| !s.eq_ignore_ascii_case("success"))
                        .unwrap_or(false)
                        || event.extra.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false)
                        || event.extra.get("error").is_some();
                    // R1: `output` is the field the cheat sheet documents for
                    // this event; `content` tolerated as a plausible alternate
                    // (mirrors the MCP/claude tool_result "content" naming).
                    let output = event
                        .extra
                        .get("output")
                        .or_else(|| event.extra.get("content"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let popped = if !id.is_empty() {
                        open_native_calls
                            .iter()
                            .rposition(|(oid, _)| oid == id)
                            .map(|pos| open_native_calls.remove(pos))
                    } else {
                        open_native_calls.pop()
                    }
                    .or_else(|| open_native_calls.pop());
                    if let Some((_, idx)) = popped {
                        if let Some(entry) = resolved.get_mut(idx) {
                            entry.2 = output;
                            entry.3 = is_error;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Convert the already id-resolved `resolved` tuples into adjacent
    // ToolUse+ToolResult chunk pairs (see doc comment above for why the
    // resolution must happen before this step).
    let mut chunks = Vec::with_capacity(resolved.len() * 2);
    for (name, input, output, is_error) in resolved {
        chunks.push(RuntimeChunk::ToolUse { name, input });
        chunks.push(RuntimeChunk::ToolResult { output, is_error });
    }

    (content, input_tokens, output_tokens, chunks)
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for GeminiRuntime {
    fn name(&self) -> &str {
        "gemini"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        info!(agent = %context.agent_id, "GeminiRuntime: executing via gemini -p --output-format stream-json");

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

        // W1 (capability enforcement): derive approval/sandbox flags from the
        // agent's capabilities instead of the former blanket `--approval-mode yolo`.
        let caps = context.capabilities.as_ref();
        if let Some(c) = caps {
            if c.has_tool_restrictions() {
                warn!(
                    runtime = "gemini",
                    agent = %context.agent_id,
                    "capability enforcement is best-effort on this runtime — \
                     per-tool allow/deny lists collapse to approval-mode/--sandbox"
                );
            }
        }

        // W2 (MCP wiring): register the duduclaw MCP server in the agent's
        // `.gemini/settings.json` before spawning (gemini reads workspace
        // settings from cwd, which we set to the agent dir below). Idempotent
        // merge; warn-not-fatal — registration failing must not block the reply.
        if let Some(ref dir) = context.agent_dir {
            if let Err(e) = Self::ensure_duduclaw_mcp_config(dir, &context.agent_id).await {
                warn!(
                    runtime = "gemini",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write gemini MCP settings — continuing without it"
                );
            }
        }

        let mut cmd = tokio::process::Command::new(&self.gemini_path);
        cmd.arg("-p")
            .arg("--output-format")
            .arg("stream-json");
        cmd.args(approval_args(caps));

        // Pass system prompt via GEMINI_SYSTEM_MD env var (temp file).
        // Gemini CLI has no --system-instruction flag.
        let _system_prompt_guard: Option<tempfile::TempPath> = if !system_prompt.is_empty() {
            match tempfile::NamedTempFile::new() {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = f.write_all(system_prompt.as_bytes());
                    let path = f.into_temp_path();
                    cmd.env("GEMINI_SYSTEM_MD", &path);
                    Some(path)
                }
                Err(_) => None,
            }
        } else {
            None
        };

        // Prepend conversation history to prompt (Gemini CLI has no native multi-turn in -p mode)
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
            cmd.current_dir(dir);
        }

        // Pass API key if available
        let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("GEMINI_API_KEY", &api_key);
        }

        // Vision: if system_prompt contains image references, pass via --include-files
        // (Gemini CLI natively supports image input)

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). Layered on top of the CLI approval/sandbox
        // flags; fail-closed if required but unavailable.
        super::apply_native_sandbox(&mut cmd, caps, context.agent_dir.as_deref(), "gemini")?;

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Gemini CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn gemini: {e}"))?;

        // Gemini exit codes: 0=ok, 1=error, 42=input error, 53=turn limit
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let hint = match code {
                42 => " (input error — check prompt format)",
                53 => " (turn limit exceeded)",
                _ => "",
            };
            return Err(format!(
                "Gemini CLI exited with {code}{hint}: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        // Parse JSONL output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let (mut content, input_tokens, output_tokens, chunks) = parse_gemini_stdout(&stdout);
        super::extend_native_tool_events(super::native_tool_events_from_chunks(&chunks));

        if content.is_empty() {
            // Fallback: use raw stdout
            content = stdout.trim().to_string();
        }

        // Still empty ⇒ FAILURE, not success: Ok("") would be silently dropped
        // by every channel and poison the session with an empty assistant turn.
        if content.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Empty response from Gemini CLI (exit 0); stderr tail: {}",
                duduclaw_core::truncate_bytes(stderr.trim(), 300)
            ));
        }

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model_used: context.model.clone(),
            runtime_name: "gemini".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.gemini_path)
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

impl GeminiRuntime {
    /// Execute and return chunks. Gemini CLI does not support true streaming,
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

impl GeminiRuntime {
    /// Write MCP server configuration to Gemini settings.
    ///
    /// If `agent_dir` is provided, writes to `agent_dir/.gemini/settings.json` for
    /// per-agent isolation. Otherwise writes to the global `~/.gemini/settings.json`.
    ///
    /// Merges per server name (other `mcpServers` entries and unrelated settings
    /// are preserved) and is idempotent: returns `Ok(false)` without writing when
    /// every requested entry already matches. Returns `Ok(true)` when written.
    pub async fn write_mcp_config(
        agent_dir: Option<&std::path::Path>,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let settings_path = if let Some(dir) = agent_dir {
            dir.join(".gemini").join("settings.json")
        } else {
            dirs::home_dir()
                .ok_or("No home dir")?
                .join(".gemini").join("settings.json")
        };
        let existing = tokio::fs::read_to_string(&settings_path)
            .await
            .unwrap_or_else(|_| "{}".to_string());
        let mut settings: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or(serde_json::json!({}));
        if !settings.is_object() {
            settings = serde_json::json!({});
        }
        let mcp = settings
            .as_object_mut()
            .expect("settings is an object — normalized above")
            .entry("mcpServers")
            .or_insert(serde_json::json!({}));
        if !mcp.is_object() {
            *mcp = serde_json::json!({});
        }
        let map = mcp.as_object_mut().expect("mcpServers normalized to object");
        let mut changed = false;
        for (name, def) in servers {
            if map.get(name) != Some(def) {
                map.insert(name.clone(), def.clone());
                changed = true;
            }
        }
        if !changed {
            return Ok(false);
        }
        if let Some(parent) = settings_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
        }
        tokio::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap_or_default(),
        )
        .await
        .map_err(|e| e.to_string())?;
        // Carries DUDUCLAW_AGENT_TOKEN in plaintext — restrict to the owning
        // OS user (0600 on Unix; no-op on Windows).
        duduclaw_core::platform::set_owner_only(&settings_path).ok();
        Ok(true)
    }

    /// W2: ensure the duduclaw MCP server (absolute binary + `mcp-server` arg +
    /// `DUDUCLAW_AGENT_ID` env) is registered in the agent's
    /// `.gemini/settings.json`. Called before every spawn; idempotent.
    pub async fn ensure_duduclaw_mcp_config(
        agent_dir: &std::path::Path,
        agent_id: &str,
    ) -> Result<bool, String> {
        let Some(def) = super::duduclaw_mcp_server_json(agent_id) else {
            return Err("duduclaw binary did not resolve to an absolute path".to_string());
        };
        let mut servers = std::collections::HashMap::new();
        servers.insert("duduclaw".to_string(), def);
        Self::write_mcp_config(Some(agent_dir), &servers).await
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gemini_result_event() {
        let line = r#"{"type":"result","content":"Hello from Gemini","stats":{"inputTokens":200,"outputTokens":80}}"#;
        let event: GeminiEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "result");
        let content = event.extra.get("content").unwrap().as_str().unwrap();
        assert_eq!(content, "Hello from Gemini");
    }

    #[test]
    fn test_parse_gemini_message_event() {
        let line = r#"{"type":"message","role":"assistant","content":"Thinking..."}"#;
        let event: GeminiEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.event_type, "message");
        assert_eq!(event.extra.get("role").unwrap().as_str().unwrap(), "assistant");
    }

    // ── T10: parse_gemini_stdout → RuntimeChunk → NativeToolEvent ───────

    #[test]
    fn parse_gemini_stdout_emits_tool_use_and_tool_result_chunks() {
        // The literal T10 ask: `RuntimeChunk::ToolUse`/`ToolResult` must
        // actually get constructed, not bypassed.
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"run_shell_command","tool_id":"tool_1","parameters":{"command":"ls"}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_1","status":"success","output":"docs\n"}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        assert_eq!(chunks.len(), 2);
        assert!(matches!(chunks[0], super::super::RuntimeChunk::ToolUse { .. }));
        match &chunks[1] {
            super::super::RuntimeChunk::ToolResult { is_error, .. } => assert!(!is_error),
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parse_gemini_stdout_collects_tool_use_paired_with_success_result() {
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"run_shell_command","tool_id":"tool_1","parameters":{"command":"ls"}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_1","status":"success","output":"docs\n"}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "run_shell_command");
        assert!(events[0].success);
    }

    #[test]
    fn parse_gemini_stdout_collects_tool_use_paired_with_error_result() {
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"read_file","tool_id":"tool_2","parameters":{"path":"missing.txt"}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_2","status":"error","output":"file not found"}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "read_file");
        assert!(!events[0].success);
    }

    #[test]
    fn parse_gemini_stdout_unpaired_tool_use_stays_provisionally_successful() {
        // A tool_use with no matching tool_result (stream truncated, or the
        // CLI omitted it) keeps its provisional success = true rather than
        // being dropped — an attempted call is still evidence.
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"write_file","tool_id":"tool_3","parameters":{}}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert!(events[0].success);
    }

    #[test]
    fn parse_gemini_stdout_multiple_tool_calls_pair_by_id_not_just_order() {
        // Two outstanding calls; the result for the FIRST arrives after the
        // second tool_use — id-based pairing must still resolve correctly
        // rather than assuming strict LIFO/FIFO order. This resolution
        // happens INSIDE parse_gemini_stdout (on the raw event stream)
        // before the result is re-expressed as adjacent RuntimeChunk pairs
        // — see the function's doc comment.
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"a","tool_id":"id-a","parameters":{}}"#,
            "\n",
            r#"{"type":"tool_use","tool_name":"b","tool_id":"id-b","parameters":{}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"id-a","status":"error","output":""}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"id-b","status":"success","output":""}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].tool_name, "a");
        assert!(!events[0].success);
        assert_eq!(events[1].tool_name, "b");
        assert!(events[1].success);
    }

    #[test]
    fn parse_gemini_stdout_mixed_events_content_and_tools() {
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"run_shell_command","tool_id":"tool_1","parameters":{}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_1","status":"success","output":""}"#,
            "\n",
            r#"{"type":"result","content":"done","stats":{"inputTokens":12,"outputTokens":7}}"#,
            "\n",
        );
        let (content, input_tokens, output_tokens, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(content, "done");
        assert_eq!(input_tokens, 12);
        assert_eq!(output_tokens, 7);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn parse_gemini_stdout_no_tool_events_is_empty() {
        let stdout = concat!(
            r#"{"type":"result","content":"just text","stats":{"inputTokens":1,"outputTokens":1}}"#,
            "\n",
        );
        let (content, _, _, chunks) = parse_gemini_stdout(stdout);
        assert_eq!(content, "just text");
        assert!(chunks.is_empty());
    }

    // ── R1: result_text / input_text capture ─────────────────────────────

    #[test]
    fn parse_gemini_stdout_captures_result_and_input_text() {
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"run_shell_command","tool_id":"tool_1","parameters":{"command":"cat report.md"}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_1","status":"success","output":"quarterly revenue: 1.2M"}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].result_text.as_deref(), Some("quarterly revenue: 1.2M"));
        assert!(events[0].input_text.as_deref().unwrap().contains("cat report.md"));
    }

    #[test]
    fn parse_gemini_stdout_output_field_alternate_content_name_is_read() {
        // `content` is the tolerated alternate field name for the result
        // text (mirrors `tool_name`/`name` and `tool_id`/`id` tolerance).
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"read_file","tool_id":"tool_1","parameters":{}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_1","status":"success","content":"file body text"}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert_eq!(events[0].result_text.as_deref(), Some("file body text"));
    }

    #[test]
    fn parse_gemini_stdout_masks_secret_in_result_text() {
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"run_shell_command","tool_id":"tool_1","parameters":{}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_1","status":"success","output":"key: sk-ant-api03-verysecretvalue1234567890"}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        let result_text = events[0].result_text.as_deref().unwrap();
        assert!(
            !result_text.contains("sk-ant-api03-verysecretvalue1234567890"),
            "secret leaked into result_text: {result_text}"
        );
    }

    #[test]
    fn parse_gemini_stdout_empty_output_stays_none() {
        let stdout = concat!(
            r#"{"type":"tool_use","tool_name":"run_shell_command","tool_id":"tool_1","parameters":{}}"#,
            "\n",
            r#"{"type":"tool_result","tool_id":"tool_1","status":"success","output":""}"#,
            "\n",
        );
        let (_, _, _, chunks) = parse_gemini_stdout(stdout);
        let events = super::super::native_tool_events_from_chunks(&chunks);
        assert!(events[0].result_text.is_none());
    }

    #[test]
    fn approval_args_default_is_auto_edit_not_yolo() {
        // Fail-closed default: never blanket-yolo without an explicit grant.
        assert_eq!(approval_args(None), vec!["--approval-mode", "auto_edit"]);
        let caps = CapabilitiesConfig::default();
        assert_eq!(
            approval_args(Some(&caps)),
            vec!["--approval-mode", "auto_edit"]
        );
    }

    #[test]
    fn approval_args_restrictive_caps_add_sandbox() {
        let caps = CapabilitiesConfig {
            allowed_tools: vec!["Read".to_string(), "Grep".to_string()],
            ..Default::default()
        };
        assert_eq!(
            approval_args(Some(&caps)),
            vec!["--approval-mode", "auto_edit", "--sandbox"]
        );
    }

    #[test]
    fn approval_args_yolo_only_on_explicit_computer_use() {
        let caps = CapabilitiesConfig {
            computer_use: true,
            ..Default::default()
        };
        assert_eq!(approval_args(Some(&caps)), vec!["--approval-mode", "yolo"]);
    }

    #[tokio::test]
    async fn mcp_config_write_merges_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join(".gemini").join("settings.json");
        std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        // Pre-existing user settings with an unrelated key + another MCP server.
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "theme": "dark",
                "mcpServers": {
                    "playwright": { "command": "npx", "args": ["mcp-playwright"] }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut servers = std::collections::HashMap::new();
        servers.insert(
            "duduclaw".to_string(),
            serde_json::json!({
                "command": "/usr/local/bin/duduclaw",
                "args": ["mcp-server"],
                "env": { "DUDUCLAW_AGENT_ID": "agnes" },
            }),
        );
        assert!(GeminiRuntime::write_mcp_config(Some(dir.path()), &servers)
            .await
            .unwrap());
        // Second call: entry already matches → no write.
        assert!(!GeminiRuntime::write_mcp_config(Some(dir.path()), &servers)
            .await
            .unwrap());

        let got: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(got["theme"], "dark", "unrelated settings preserved");
        assert_eq!(
            got["mcpServers"]["playwright"]["command"], "npx",
            "other MCP servers preserved"
        );
        assert_eq!(
            got["mcpServers"]["duduclaw"]["env"]["DUDUCLAW_AGENT_ID"], "agnes",
            "duduclaw entry carries the agent identity env"
        );
    }
}
