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
        let mut content = String::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        for line in stdout.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<CodexEvent>(line) {
                match event.event_type.as_str() {
                    "item.completed" => {
                        // Extract text from message items
                        if let Some(item) = event.extra.get("item") {
                            if item.get("type").and_then(|t| t.as_str()) == Some("message") {
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

        if content.is_empty() {
            // Fallback: use the last line as content
            content = stdout.lines().last().unwrap_or("").to_string();
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
