//! Google Antigravity CLI runtime — `agy -p <prompt>`.
//!
//! Background: on 2026-06-18 Google retired the personal-tier `gemini` CLI and
//! replaced it with the **Antigravity CLI** (`agy`), a Go single-binary that
//! supersedes `gemini-cli`. It shares the `~/.gemini/` lineage (config now under
//! `~/.gemini/antigravity-cli/`) and exposes Gemini 3.x plus Claude / GPT-OSS
//! models behind one terminal agent. This runtime drives that binary.
//!
//! Differences from [`super::gemini::GeminiRuntime`] (the legacy backend, kept for
//! paid `GEMINI_API_KEY` users whose access continues past the shutdown):
//!   - binary `agy` (installed to `~/.local/bin/agy`), not `gemini`
//!   - model flag `--model <id>`, not `-m <id>`
//!   - permission bypass `--dangerously-skip-permissions`, not `--approval-mode yolo`
//!   - API key env `ANTIGRAVITY_API_KEY`, not `GEMINI_API_KEY`
//!   - MCP config under `~/.gemini/antigravity-cli/settings.json`
//!
//! Verified against `agy --help` + live runs (v1.0.12, 2026-06-25). Confirmed facts:
//!   - `-p` / `--print` takes the prompt as its **value** (not a boolean) and
//!     must be the LAST flag — it consumes the next argv token as the prompt, so
//!     any flag after `-p` is swallowed as the prompt. All other flags go first.
//!   - `--dangerously-skip-permissions` auto-approves all tool permission
//!     requests without prompting. As of 2026-07-06 this is NO LONGER emitted
//!     unconditionally: `agy --help` confirms a `--sandbox` flag exists, so the
//!     runtime now derives confinement from `CapabilitiesConfig` (see
//!     `sandbox_args`) — skip-permissions only on an explicit FullAccess grant.
//!   - `--model <id>` selects the session model; `--add-dir <path>` adds a
//!     workspace dir (we point it at the agent home so `agy` does not silently
//!     create a default `~/.gemini/antigravity-cli/scratch/` workspace).
//!   - `--print-timeout` bounds print-mode wait (CLI default 5m). We set it
//!     explicitly and keep the wrapper timeout a notch higher as a backstop.
//!   - v1.2.10 exposes `--output-format stream-json` with final usage and
//!     `step_update` tool events. There is no `--system` flag, so we embed
//!     the system prompt + history *inside the prompt argument*,
//!     guaranteeing the model receives them. The 64KB system-prompt cap keeps
//!     the argv well under ARG_MAX.

use async_trait::async_trait;
use serde_json::Value;
use tracing::info;

use duduclaw_core::types::{CapabilitiesConfig, SandboxLevel, sandbox_level_for};

use super::{AgentRuntime, RuntimeContext, RuntimeResponse};

/// Derive agy sandbox / permission flags from the agent's capabilities.
///
/// agy exposes exactly two relevant flags (verified via real `agy --help`,
/// 2026-07-06 — supersedes the stale v1.0.12 "no sandbox flags" note):
///   - `--sandbox`                      Run in a sandbox with terminal restrictions enabled
///   - `--dangerously-skip-permissions` Auto-approve all tool permission requests
///
/// Mapping mirrors the codex/gemini `SandboxLevel` enforcement:
///   - `ReadOnly` / `WorkspaceWrite`          → `--sandbox` (confine blast radius)
///   - `FullAccess` (explicit `computer_use`) → `--dangerously-skip-permissions`
///
/// HARD RULE (upstream issue #36): never emit both. Combining `--sandbox` with
/// `--dangerously-skip-permissions` auto-approves attempts to escape the
/// sandbox, defeating the confinement.
fn sandbox_args(caps: Option<&CapabilitiesConfig>) -> Vec<String> {
    match sandbox_level_for(caps) {
        SandboxLevel::FullAccess => vec!["--dangerously-skip-permissions".to_string()],
        SandboxLevel::ReadOnly | SandboxLevel::WorkspaceWrite => vec!["--sandbox".to_string()],
    }
}

/// `--effort <low|medium|high>` for one invocation (P1/WP-3).
///
/// Verified on agy 1.2.10 (`--help`): "Reasoning effort for the current CLI
/// session (low|medium|high)". There is no xhigh/max, so
/// `Effort::clamp_for` folds those down to `high` rather than sending a value
/// agy would reject. `None` ⇒ empty.
fn effort_args(effort: Option<duduclaw_core::effort::Effort>, model: &str) -> Vec<String> {
    // agy 1.2.10 refuses gemini-3.7-flash without an explicit effort, even
    // though --effort is optional in --help. A matrix cell has no per-role
    // config to supply one, so use the production-like medium default for
    // this model family. Keep every other model's old argv unchanged.
    let effort = effort.or_else(|| {
        model
            .starts_with("gemini-3.7-")
            .then_some(duduclaw_core::effort::Effort::Medium)
    });
    match effort {
        Some(e) => vec![
            "--effort".to_string(),
            e.clamp_for(duduclaw_core::types::RuntimeType::Antigravity)
                .as_str()
                .to_string(),
        ],
        None => Vec::new(),
    }
}

/// One decoded `agy --output-format stream-json` run.
#[derive(Debug, Default)]
struct ParsedStream {
    content: String,
    /// `(input, output, cache_read)` — `None` when the stream carried no
    /// usable usage block. Deliberately an `Option` rather than three zeros:
    /// "agy did not report usage" and "agy reported zero tokens" are different
    /// facts, and the cost ledger must not be fed an invented number.
    usage: Option<(u64, u64, u64)>,
    tools: Vec<super::NativeToolEvent>,
    /// Set when the strict 1.2.10 shape was not found and this parse fell back.
    /// Logged once by the caller; never hidden.
    degraded: Option<&'static str>,
}

/// Decode the observed agy 1.2.10 NDJSON stream. Only terminal tool states
/// are evidence: ACTIVE is an attempted call, not proof that it ran. Unknown
/// shapes are ignored.
///
/// **Degradation (2026-09-28 review, decided).** This parser used to be strict
/// all the way down: a single unparseable line, a missing `result` event, a
/// missing `response` field, or a `usage` block short one integer turned the
/// whole run into `Err` — and because `execute()` propagates that, an agy that
/// had *already answered* was reported as a failed spawn and the whole role
/// member was lost. Six independent hard failures for one CLI whose output
/// shape is pinned to the single version this was written against.
///
/// Now: shape mismatches degrade, facts do not.
/// * an unparseable line is skipped (it is one line of NDJSON, not the run);
/// * no `result` event, or a `result` with no `response`, falls back to the
///   last non-empty line as the answer;
/// * a missing or partial `usage` block yields `usage: None` — the answer is
///   kept, the tokens are reported as unknown rather than as zero;
/// * an *explicit* non-`SUCCESS` `status` is still a hard error. That is agy
///   telling us the run failed, not a shape we failed to recognise — the one
///   case where refusing is the honest answer. An ABSENT `status` is a shape
///   question and degrades like the rest.
fn parse_stream_output(raw: &str) -> Result<ParsedStream, String> {
    let mut result: Option<Value> = None;
    let mut tools = std::collections::BTreeMap::<u64, super::NativeToolEvent>::new();
    let mut unparseable_lines = 0usize;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            unparseable_lines += 1;
            continue;
        };
        match event.get("event").and_then(Value::as_str) {
            Some("result") => result = event.get("result").cloned(),
            Some("step_update") => {
                let Some(step) = event.get("step_update") else {
                    continue;
                };
                if step.get("step_type").and_then(Value::as_str) != Some("tool") {
                    continue;
                }
                let Some(index) = step.get("step_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(state) = step.get("state").and_then(Value::as_str) else {
                    continue;
                };
                if state != "DONE" && state != "ERROR" {
                    continue;
                }
                let Some(name) = step.get("tool_name").and_then(Value::as_str) else {
                    continue;
                };
                let info = step.get("tool_info");
                let input_text = info
                    .and_then(|i| i.get("parameters"))
                    .and_then(super::native_event_input_text_from_value);
                let result_text = info
                    .and_then(|i| i.get("error").or_else(|| i.get("result")))
                    .and_then(|v| serde_json::to_string(v).ok())
                    .and_then(|s| super::native_event_result_text(&s));
                tools.insert(
                    index,
                    super::NativeToolEvent {
                        tool_name: name.to_string(),
                        success: state == "DONE",
                        result_text,
                        input_text,
                    },
                );
            }
            _ => {}
        }
    }
    let tools: Vec<super::NativeToolEvent> = tools.into_values().collect();

    // The one hard failure that survives: agy explicitly said the run failed.
    if let Some(status) = result
        .as_ref()
        .and_then(|r| r.get("status"))
        .and_then(Value::as_str)
    {
        if status != "SUCCESS" {
            return Err(format!("Antigravity result status was {status}, not SUCCESS"));
        }
    }

    let response = result
        .as_ref()
        .and_then(|r| r.get("response"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let usage = result.as_ref().and_then(|r| r.get("usage")).and_then(|u| {
        // Both counters or neither: a half-populated block is not a usage
        // report, and pairing a real input count with a fabricated zero output
        // count would feed the cost ledger a lie.
        let input = u.get("input_tokens").and_then(Value::as_u64)?;
        let output = u.get("output_tokens").and_then(Value::as_u64)?;
        let cache = u
            .get("cache_read_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Some((input, output, cache))
    });

    let (content, mut degraded) = match response {
        Some(c) => (c, None),
        None => {
            // Last non-empty line as the answer — the same last-resort the
            // codex runtime already uses. Refusing here would throw away a
            // reply agy did produce.
            let last = raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .next_back()
                .unwrap_or_default()
                .to_string();
            if last.is_empty() {
                return Err(
                    "Antigravity stream carried neither a result response nor any output"
                        .to_string(),
                );
            }
            (
                last,
                Some("no result/response event — fell back to the last stream line"),
            )
        }
    };
    if degraded.is_none() && usage.is_none() {
        degraded = Some("result carried no usable usage block — tokens reported as unknown");
    }
    if degraded.is_none() && unparseable_lines > 0 {
        degraded = Some("some stream lines were not valid JSON and were skipped");
    }

    Ok(ParsedStream {
        content,
        usage,
        tools,
        degraded,
    })
}

/// Hard backstop on the whole subprocess. Kept a notch above `PRINT_TIMEOUT`
/// (agy's own print-mode wait) so agy self-bounds first and this only fires if
/// the process truly wedges.
const DEFAULT_TIMEOUT_SECS: u64 = 330;
/// Value passed to `agy --print-timeout` (agy's CLI default is 5m).
const PRINT_TIMEOUT: &str = "300s";
/// Cap the system prompt embedded into the prompt argument (ARG_MAX safety).
const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;

/// Runtime that delegates to the Google Antigravity CLI (`agy`).
pub struct AntigravityRuntime {
    agy_path: String,
}

impl AntigravityRuntime {
    pub fn new() -> Self {
        Self {
            agy_path: resolve_agy_path(),
        }
    }
}

impl Default for AntigravityRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve the `agy` binary. Prefer a bare `agy` on `$PATH`; fall back to the
/// documented install location `~/.local/bin/agy` so launchd/systemd-launched
/// gateways (which often lack the interactive `PATH`) still discover it. The
/// availability probe ultimately decides whether the runtime registers, so a
/// stale guess here is harmless.
fn resolve_agy_path() -> String {
    if let Some(home) = dirs::home_dir() {
        let local = home.join(".local").join("bin").join("agy");
        if local.is_file() {
            return local.to_string_lossy().into_owned();
        }
    }
    "agy".to_string()
}

/// Idempotently add `dir` to agy's `trustedWorkspaces` so that running there does
/// not trigger the interactive "trust this workspace?" prompt (which would hang a
/// headless subprocess). Writes the global `~/.gemini/antigravity-cli/settings.json`
/// under a cross-process lock (multiple agents may share it). Best-effort: any IO
/// error is returned for the caller to log, never to abort the agent call.
fn ensure_workspace_trusted(dir: &std::path::Path) -> std::io::Result<()> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let settings_path = home
        .join(".gemini")
        .join("antigravity-cli")
        .join("settings.json");
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Canonicalize so the stored path matches what agy compares against.
    let target = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .into_owned();

    duduclaw_core::with_file_lock(&settings_path, || {
        let existing = std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".to_string());
        let mut settings: serde_json::Value =
            serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!({}));
        let mut list: Vec<serde_json::Value> = settings
            .get("trustedWorkspaces")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if list.iter().any(|v| v.as_str() == Some(target.as_str())) {
            return Ok(()); // already trusted — no write
        }
        list.push(serde_json::Value::String(target.clone()));
        settings["trustedWorkspaces"] = serde_json::Value::Array(list);
        let out = serde_json::to_string_pretty(&settings).unwrap_or_default();
        std::fs::write(&settings_path, out)
    })
}

/// Build the prompt payload: system instructions + history + user message, all
/// embedded as text. Pure function so it is unit-testable without spawning `agy`.
fn build_prompt(context: &RuntimeContext, user_prompt: &str) -> String {
    // Limit system_prompt to 64KB to avoid ARG_MAX issues.
    let system_prompt: &str = if context.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        // Walk back to a char boundary so multi-byte CJK/emoji never split.
        let cut = duduclaw_core::truncate_bytes(&context.system_prompt, MAX_SYSTEM_PROMPT_BYTES);
        cut
    } else {
        &context.system_prompt
    };

    // Prevent argument injection: a prompt starting with '-' would be parsed as a flag.
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
        // Escape closing tag in the system prompt to keep the XML frame intact.
        let safe_system =
            system_prompt.replace("</system_instructions>", "&lt;/system_instructions&gt;");
        format!("<system_instructions>\n{safe_system}\n</system_instructions>\n\n{with_history}")
    }
}

// ── AgentRuntime impl ───────────────────────────────────────────

#[async_trait]
impl AgentRuntime for AntigravityRuntime {
    fn name(&self) -> &str {
        "antigravity"
    }

    async fn execute(
        &self,
        prompt: &str,
        context: &RuntimeContext,
    ) -> Result<RuntimeResponse, String> {
        info!(agent = %context.agent_id, "AntigravityRuntime: executing via agy -p");

        // P0-3: agy DOES expose `--sandbox` (verified via real `agy --help`,
        // 2026-07-06 — the old v1.0.12 "no sandbox flags" claim is obsolete).
        // Derive the confinement flags from the agent's CapabilitiesConfig so
        // this runtime enforces SandboxLevel just like codex/gemini, instead of
        // unconditionally auto-approving every tool call.
        let sb_args = sandbox_args(context.capabilities.as_ref());
        info!(
            runtime = "antigravity",
            agent = %context.agent_id,
            sandbox_flags = ?sb_args,
            "antigravity sandbox flags derived from capabilities"
        );

        // W2 (MCP wiring): register the duduclaw MCP server in the agent's
        // antigravity settings before spawning. Idempotent merge;
        // warn-not-fatal — registration failing must not block the reply.
        if let Some(ref dir) = context.agent_dir {
            if let Err(e) =
                Self::ensure_duduclaw_mcp_config(dir, &context.agent_id, &context.home_dir).await
            {
                tracing::warn!(
                    runtime = "antigravity",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write antigravity MCP settings — continuing without it"
                );
            }
        }

        let payload = build_prompt(context, prompt);

        let mut cmd = tokio::process::Command::new(&self.agy_path);
        // CRITICAL ordering: `-p`/`--print` is NOT a boolean — it consumes the
        // *next argv token* as the prompt value (verified: `agy -p` alone errors
        // "flag needs an argument: -p"). So every other flag MUST come first and
        // `-p <payload>` MUST be last; otherwise `-p` swallows the following flag
        // as the prompt and the real payload is dropped (the cause of agy
        // "answering" about whatever flag followed `-p`).
        //
        // Capability-derived sandbox/permission flags (see `sandbox_args`):
        // `--sandbox` for restricted levels, `--dangerously-skip-permissions`
        // only on an explicit FullAccess grant — never both. `--print-timeout`
        // bounds agy's own wait. All flags MUST precede `-p <payload>`.
        for a in &sb_args {
            cmd.arg(a);
        }
        cmd.arg("--print-timeout").arg(PRINT_TIMEOUT);
        cmd.arg("--output-format").arg("stream-json");

        // P1/WP-3: per-call reasoning effort. Verified on agy 1.2.10 —
        // `--effort   Reasoning effort for the current CLI session (low|medium|high)`.
        // No xhigh/max exists here, so `clamp_for` folds both down to `high`.
        // `None` ⇒ flag absent, argv byte-identical to before.
        for a in effort_args(context.effort, &context.model) {
            cmd.arg(a);
        }

        // Set model if specified (agy uses `--model`, not `-m`).
        if !context.model.is_empty() {
            cmd.arg("--model").arg(&context.model);
        }

        // Point agy at the working root as its workspace so it does not silently
        // spin up a default `~/.gemini/antigravity-cli/scratch/` project.
        //
        // Working root: normally the agent's own directory, but a caller may
        // override the cwd via `super::SPAWN_OVERRIDE` (today: the team
        // composer, putting a role member in the employee's workspace so its
        // files outlive the throwaway scaffold — design §4.3 E3). Until the
        // 2026-09-28 review this runtime ignored the override, so an agy role
        // member's work was deleted by the immediate GC. Identity is NOT
        // affected: the MCP settings written above stay keyed to `agent_dir`.
        //
        // CRITICAL: agy shows an *interactive* "trust this workspace?" prompt for
        // any dir not in `trustedWorkspaces`. In a headless subprocess (no TTY)
        // that prompt blocks forever — `--dangerously-skip-permissions` only
        // auto-approves *tool* calls, not workspace trust. So we pre-seed the
        // working root (the dir agy actually opens) into agy's settings before
        // spawning. Best-effort: a failure here just risks the prompt, it must
        // not abort the call.
        let work_root: Option<std::path::PathBuf> =
            super::resolve_spawn_work_dir(context.agent_dir.as_deref(), &context.agent_id);
        if let Some(ref dir) = work_root {
            let d = dir.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || ensure_workspace_trusted(&d)).await
            {
                tracing::warn!(agent = %context.agent_id, error = %e, "ensure_workspace_trusted join failed");
            }
            cmd.arg("--add-dir").arg(dir);
            cmd.current_dir(dir);
        }

        // Prompt LAST, as the value of `-p` (see ordering note above).
        cmd.arg("-p").arg(&payload);

        // Pass API key if available (Antigravity's own env var).
        let api_key = std::env::var("ANTIGRAVITY_API_KEY").unwrap_or_default();
        if !api_key.is_empty() {
            cmd.env("ANTIGRAVITY_API_KEY", &api_key);
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Native OS sandbox (opt-in). agy has no CLI sandbox flags, so this OS
        // floor is the only enforceable confinement on this runtime; fail-closed
        // if required but unavailable.
        // Scoped to the working root so an overridden cwd is the directory that
        // gets write access — same rule as `runtime/codex.rs`.
        super::apply_native_sandbox(
            &mut cmd,
            context.capabilities.as_ref(),
            work_root.as_deref(),
            "antigravity",
        )?;

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Antigravity CLI timed out".to_string())?
        .map_err(|e| format!("Failed to spawn agy: {e}"))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Antigravity CLI exited with {code}: {}",
                stderr.chars().take(500).collect::<String>()
            ));
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let parsed = parse_stream_output(&raw)?;
        if let Some(reason) = parsed.degraded {
            tracing::warn!(
                runtime = "antigravity",
                agent = %context.agent_id,
                reason = %reason,
                "agy stream did not match the 1.2.10 shape — degraded parse (the reply is \
                 kept; usage may be reported as zero)"
            );
        }
        let ParsedStream {
            content,
            usage,
            tools: native_events,
            ..
        } = parsed;
        // Unknown usage is carried as zeros because `RuntimeResponse` has no
        // "unknown" representation; the `warn!` above is what distinguishes it
        // from a genuine zero-token run.
        let (input_tokens, output_tokens, cache_read_tokens) = usage.unwrap_or((0, 0, 0));
        super::extend_native_tool_events(native_events);

        // Empty stdout with exit 0 is a FAILURE: an Ok("") would be silently
        // dropped by every channel and poison the session with an empty
        // assistant turn.
        if content.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Empty response from Antigravity CLI (exit 0); stderr tail: {}",
                duduclaw_core::truncate_bytes(stderr.trim(), 300)
            ));
        }

        Ok(RuntimeResponse {
            content,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            model_used: context.model.clone(),
            runtime_name: "antigravity".to_string(),
        })
    }

    async fn is_available(&self) -> bool {
        tokio::process::Command::new(&self.agy_path)
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

impl AntigravityRuntime {
    /// Execute and return chunks. `agy -p` is request/response, so this wraps the
    /// normal execution into a single `Done` chunk (mirrors the Gemini backend).
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

impl AntigravityRuntime {
    /// Write MCP server configuration to Antigravity settings.
    ///
    /// If `agent_dir` is provided, writes to
    /// `agent_dir/.gemini/antigravity-cli/settings.json` for per-agent isolation.
    /// Otherwise writes to the global `~/.gemini/antigravity-cli/settings.json`.
    ///
    /// Merges per server name (other `mcpServers` entries and unrelated settings —
    /// e.g. `trustedWorkspaces` — are preserved) and is idempotent: returns
    /// `Ok(false)` without writing when every requested entry already matches.
    pub async fn write_mcp_config(
        agent_dir: Option<&std::path::Path>,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let settings_path = if let Some(dir) = agent_dir {
            dir.join(".gemini")
                .join("antigravity-cli")
                .join("settings.json")
        } else {
            dirs::home_dir()
                .ok_or("No home dir")?
                .join(".gemini")
                .join("antigravity-cli")
                .join("settings.json")
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
        let map = mcp
            .as_object_mut()
            .expect("mcpServers normalized to object");
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
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| e.to_string())?;
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
    /// `DUDUCLAW_AGENT_ID` env) is registered in the agent's antigravity
    /// settings. Called before every spawn; idempotent.
    pub async fn ensure_duduclaw_mcp_config(
        agent_dir: &std::path::Path,
        agent_id: &str,
        home_dir: &std::path::Path,
    ) -> Result<bool, String> {
        let Some(def) = super::duduclaw_mcp_server_json_for_home(agent_id, home_dir) else {
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
    fn stream_result_reports_measured_usage_and_terminal_tool_event() {
        let raw = concat!(
            "{\"event\":\"step_update\",\"step_update\":{\"step_index\":2,\"step_type\":\"tool\",\"state\":\"ACTIVE\",\"tool_name\":\"run_command\"}}\n",
            "{\"event\":\"step_update\",\"step_update\":{\"step_index\":2,\"step_type\":\"tool\",\"state\":\"ERROR\",\"tool_name\":\"run_command\",\"tool_info\":{\"parameters\":{\"CommandLine\":\"pwd\"},\"error\":{\"type\":\"TOOL_ERROR\"}}}}\n",
            "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"PING\\n\",\"usage\":{\"input_tokens\":42,\"output_tokens\":7,\"cache_read_tokens\":3}}}\n"
        );
        let parsed = parse_stream_output(raw).unwrap();
        assert_eq!(parsed.content, "PING");
        assert_eq!(parsed.usage, Some((42, 7, 3)));
        assert!(
            parsed.degraded.is_none(),
            "the exact 1.2.10 shape must not report a degrade: {:?}",
            parsed.degraded
        );
        let events = &parsed.tools;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].tool_name, "run_command");
        assert!(!events[0].success);
        assert!(events[0].input_text.as_deref().unwrap().contains("pwd"));
    }

    // ── 2026-09-28 review: the strict parser had no degradation path ─────────

    /// Regression: a stream with no `result` event used to be a hard `Err`,
    /// which `execute()` propagated as a failed spawn — throwing away an answer
    /// agy had already produced just because the framing was not the one shape
    /// this parser was written against.
    #[test]
    fn a_stream_without_a_result_event_degrades_to_the_last_line_instead_of_failing() {
        let raw = concat!(
            "{\"event\":\"init\"}\n",
            "The answer is 42.\n"
        );
        let parsed = parse_stream_output(raw).expect("an unknown shape must not fail the spawn");
        assert_eq!(parsed.content, "The answer is 42.");
        assert_eq!(
            parsed.usage, None,
            "usage must be absent, never invented as zero-with-no-warning"
        );
        assert!(parsed.degraded.is_some(), "the degrade must be reported");
    }

    /// Regression: a complete answer with a `usage` block short one integer
    /// used to lose the whole run. The answer is the valuable part; the token
    /// counts are telemetry.
    #[test]
    fn a_result_with_incomplete_usage_keeps_the_answer_and_reports_unknown_tokens() {
        let raw = "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"done\",\"usage\":{\"input_tokens\":42}}}\n";
        let parsed = parse_stream_output(raw).unwrap();
        assert_eq!(parsed.content, "done");
        assert_eq!(parsed.usage, None);
        assert!(parsed.degraded.is_some());

        // No usage block at all: same rule.
        let raw = "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"done\"}}\n";
        let parsed = parse_stream_output(raw).unwrap();
        assert_eq!(parsed.content, "done");
        assert_eq!(parsed.usage, None);
    }

    /// One malformed NDJSON line is one line, not the run.
    #[test]
    fn an_unparseable_line_is_skipped_not_fatal() {
        let raw = concat!(
            "not json at all\n",
            "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}}\n",
        );
        let parsed = parse_stream_output(raw).unwrap();
        assert_eq!(parsed.content, "ok");
        assert_eq!(parsed.usage, Some((1, 2, 0)));
    }

    /// The one hard failure that survives the degradation: agy explicitly
    /// saying the run failed is a fact, not a shape we failed to recognise.
    #[test]
    fn an_explicit_non_success_status_is_still_an_error() {
        let raw = "{\"event\":\"result\",\"result\":{\"status\":\"FAILED\",\"response\":\"partial\"}}\n";
        let err = parse_stream_output(raw).unwrap_err();
        assert!(err.contains("FAILED"), "{err}");
    }

    #[test]
    fn an_empty_stream_is_still_an_error() {
        // Nothing to salvage ⇒ an honest failure, never `Ok("")`.
        assert!(parse_stream_output("").is_err());
        assert!(parse_stream_output("   \n\n").is_err());
    }
    use crate::runtime::ConversationTurn;

    fn ctx(system: &str, model: &str) -> RuntimeContext {
        RuntimeContext {
            agent_dir: None,
            system_prompt: system.to_string(),
            model: model.to_string(),
            max_tokens: 4096,
            home_dir: std::path::PathBuf::from("/tmp"),
            agent_id: "test".to_string(),
            preferred_provider: None,
            conversation_history: vec![],
            capabilities: None,
            account_pool: vec![],
            effort: None,
            allow_cross_family_failover: true,
        }
    }

    // ── P0-3: capability-derived sandbox/permission flags ─────────────────────

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
    fn effort_args_clamp_to_high_and_are_empty_when_none() {
        use duduclaw_core::effort::Effort;
        // None ⇒ byte-identical argv.
        assert!(effort_args(None, "older-model").is_empty());
        assert_eq!(
            effort_args(None, "gemini-3.7-flash"),
            vec!["--effort", "medium"]
        );
        assert_eq!(
            effort_args(Some(Effort::Low), "gemini-3.7-flash"),
            vec!["--effort", "low"]
        );
        assert_eq!(
            effort_args(Some(Effort::High), "older-model"),
            vec!["--effort", "high"]
        );
        // agy 1.2.10 accepts only low|medium|high — xhigh/max must clamp, not
        // be forwarded as a value the CLI would reject.
        assert_eq!(
            effort_args(Some(Effort::XHigh), "older-model"),
            vec!["--effort", "high"]
        );
        assert_eq!(
            effort_args(Some(Effort::Max), "older-model"),
            vec!["--effort", "high"]
        );
    }

    #[test]
    fn sandbox_args_none_caps_is_sandbox() {
        // None caps ⇒ WorkspaceWrite ⇒ confine, not auto-approve.
        assert_eq!(sandbox_args(None), vec!["--sandbox"]);
    }

    #[test]
    fn sandbox_args_default_caps_is_sandbox() {
        let c = caps(false, false, &[], &[]);
        assert_eq!(sandbox_args(Some(&c)), vec!["--sandbox"]);
    }

    #[test]
    fn sandbox_args_read_only_is_sandbox() {
        let c = caps(false, false, &["Read", "Grep"], &[]);
        assert_eq!(sandbox_args(Some(&c)), vec!["--sandbox"]);
    }

    #[test]
    fn sandbox_args_full_access_is_skip_permissions_only() {
        let c = caps(true, false, &[], &[]);
        assert_eq!(
            sandbox_args(Some(&c)),
            vec!["--dangerously-skip-permissions"]
        );
    }

    #[test]
    fn sandbox_args_never_emits_both_flags() {
        // HARD RULE (issue #36): the two flags are mutually exclusive in every level.
        for c in [
            sandbox_args(None),
            sandbox_args(Some(&caps(false, false, &[], &[]))),
            sandbox_args(Some(&caps(false, false, &["Read"], &[]))),
            sandbox_args(Some(&caps(true, false, &[], &[]))),
        ] {
            let has_sandbox = c.iter().any(|a| a == "--sandbox");
            let has_skip = c.iter().any(|a| a == "--dangerously-skip-permissions");
            assert!(
                !(has_sandbox && has_skip),
                "--sandbox and --dangerously-skip-permissions must never coexist: {c:?}"
            );
        }
    }

    #[test]
    fn build_prompt_wraps_system_instructions() {
        let c = ctx("You are helpful.", "gemini-3-pro");
        let out = build_prompt(&c, "Hello");
        assert!(out.contains("<system_instructions>"));
        assert!(out.contains("You are helpful."));
        assert!(out.contains("Hello"));
    }

    #[test]
    fn build_prompt_no_system_is_plain() {
        let c = ctx("", "");
        let out = build_prompt(&c, "Just this");
        assert_eq!(out, "Just this");
    }

    #[test]
    fn build_prompt_neutralizes_leading_dash() {
        let c = ctx("", "");
        let out = build_prompt(&c, "--help me");
        assert!(
            out.starts_with(' '),
            "leading dash must be neutralized: {out:?}"
        );
    }

    #[test]
    fn build_prompt_includes_history() {
        let mut c = ctx("sys", "");
        c.conversation_history = vec![ConversationTurn {
            role: "user".to_string(),
            content: "prior".to_string(),
        }];
        let out = build_prompt(&c, "now");
        assert!(out.contains("<conversation_history>"));
        assert!(out.contains("prior"));
        assert!(out.contains("now"));
    }

    #[test]
    fn build_prompt_truncates_oversized_system_on_char_boundary() {
        // 70KB of a 3-byte CJK char — must not panic and must stay valid UTF-8.
        let big = "中".repeat(70_000 / 3);
        let c = ctx(&big, "");
        let out = build_prompt(&c, "x");
        assert!(out.is_char_boundary(out.len()));
        assert!(out.contains("x"));
    }

    /// End-to-end against the real `agy` binary. Ignored by default (needs the
    /// CLI installed + authenticated). Run with:
    ///   DUDUCLAW_AGY_E2E=1 cargo test -p duduclaw-gateway --lib \
    ///     antigravity::tests::e2e_real_agy -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a live, authenticated `agy` CLI"]
    async fn e2e_real_agy() {
        if std::env::var("DUDUCLAW_AGY_E2E").as_deref() != Ok("1") {
            eprintln!("set DUDUCLAW_AGY_E2E=1 to run this e2e");
            return;
        }
        let rt = AntigravityRuntime::new();
        assert!(
            rt.is_available().await,
            "agy not found on PATH/~/.local/bin"
        );

        let dir = std::env::temp_dir().join("duduclaw-agy-e2e");
        let _ = std::fs::create_dir_all(&dir);
        let c = RuntimeContext {
            agent_dir: Some(dir),
            system_prompt: "You are a terse echo bot. Reply with one word only.".to_string(),
            model: String::new(),
            max_tokens: 256,
            home_dir: std::path::PathBuf::from("/tmp"),
            agent_id: "e2e".to_string(),
            preferred_provider: None,
            conversation_history: vec![],
            capabilities: None,
            account_pool: vec![],
            effort: None,
            allow_cross_family_failover: true,
        };
        let resp = rt
            .execute("Reply with exactly: PONG", &c)
            .await
            .expect("agy execute failed");
        eprintln!("agy responded: {:?}", resp.content);
        assert!(!resp.content.is_empty(), "empty response from agy");
        assert_eq!(resp.runtime_name, "antigravity");
    }
}
