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
//!   - API-key route: `GEMINI_API_KEY` in the env AND `"modelProvider": "gemini"`
//!     in `~/.gemini/antigravity-cli/settings.json`, opted into explicitly via
//!     `config.toml [antigravity] auth = "api_key"` (agy 1.1.13+; there is no
//!     `ANTIGRAVITY_API_KEY` — the variable this runtime used to forward never
//!     existed, see `commercial/docs/REPORT-antigravity-runtime-auth-2026-10.md`)
//!   - MCP config in `<workspace>/.agents/mcp_config.json` (agy ignores
//!     `mcpServers` in any `settings.json`); identity rides the spawn env
//!
//! See [`super::antigravity_setup`] for the auth / MCP / env decisions.
//!
//! Originally written against `agy --help` + live runs (v1.0.12, 2026-06-25);
//! the flags below were re-checked on 1.2.10–1.2.14. Confirmed facts:
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

use super::antigravity_setup as setup;
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
    /// agy refused at least one tool in this run although it still answered
    /// (redacted [`denial_summary`]). The caller logs it; the reply is kept.
    denied: Option<String>,
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
    parse_stream_output_keyed(raw, None)
}

/// [`parse_stream_output`] with the run's key, for redacting error text.
///
/// Every agy-supplied fragment (`error`, denial labels) is redacted BEFORE it
/// is capped, so a cut can never leave a key prefix behind; the whole message
/// is redacted once more at the end.
fn parse_stream_output_keyed(raw: &str, key: Option<&str>) -> Result<ParsedStream, String> {
    parse_stream_output_inner(raw, key).map_err(|e| setup::redact_key(&e, key))
}

fn parse_stream_output_inner(raw: &str, key: Option<&str>) -> Result<ParsedStream, String> {
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
            // agy's own `error` text is kept as secondary detail: redacted,
            // then capped.
            let detail = result
                .as_ref()
                .and_then(|r| r.get("error"))
                .and_then(Value::as_str)
                .map(|e| format!(": {}", redact_then_cap(e.trim(), key, 300)))
                .unwrap_or_default();
            return Err(match result.as_ref().and_then(|r| denial_summary(r, key)) {
                Some(denial) => format!(
                    "{denial}; Antigravity result status was {status}, not SUCCESS{detail}"
                ),
                None => format!("Antigravity result status was {status}, not SUCCESS{detail}"),
            });
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
        // A denied tool ended the turn without an answer: falling back to the
        // last stream line would hand the raw result event over as the reply.
        None if result.as_ref().and_then(|r| denial_summary(r, key)).is_some() => {
            return Err(result
                .as_ref()
                .and_then(|r| denial_summary(r, key))
                .unwrap_or_default());
        }
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

    let denied = result.as_ref().and_then(|r| denial_summary(r, key));
    Ok(ParsedStream {
        content,
        usage,
        tools,
        degraded,
        denied,
    })
}

/// The tools agy's print mode refused, from `result.denied_actions`, as one
/// sentence — or `None` when nothing was denied.
///
/// Why: a soft-denied tool confirmation ends the turn, but agy reports that
/// only in `denied_actions`; `status`/`error` (and stderr) may carry an
/// unrelated earlier message, e.g. a transient 503 from a retried attempt,
/// which made the failure look like a capacity problem. Only agy's fixed
/// `display_name`/`action` labels are used (each redacted, then capped), never
/// tool input.
fn denial_summary(result: &Value, key: Option<&str>) -> Option<String> {
    let denied = result.get("denied_actions")?.as_array()?;
    let names: Vec<String> = denied
        .iter()
        .filter_map(|d| {
            let label = d
                .get("display_name")
                .and_then(Value::as_str)
                .or_else(|| d.get("action").and_then(Value::as_str))?;
            let label = redact_then_cap(label.trim(), key, 64);
            if label.is_empty() {
                return None;
            }
            Some(match d.get("action").and_then(Value::as_str) {
                Some(kind) if kind != label => {
                    format!("{label} ({})", redact_then_cap(kind.trim(), key, 32))
                }
                _ => label.to_string(),
            })
        })
        .take(8)
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "agy denied a tool permission it could not ask about in print mode: {} \
         (this employee's capability level does not allow it unattended)",
        names.join(", ")
    ))
}

/// Redact the key (and any Google-key shape) from `text`, THEN cap it at
/// `max_chars` — the other order could cut through a key and leave a prefix no
/// redaction pass recognises.
fn redact_then_cap(text: &str, key: Option<&str>, max_chars: usize) -> String {
    duduclaw_core::truncate_chars(&setup::redact_key(text, key), max_chars)
}

/// [`denial_summary`] of the last `result` event in a raw stream.
fn stream_denial_summary(raw: &str, key: Option<&str>) -> Option<String> {
    raw.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e.get("event").and_then(Value::as_str) == Some("result"))
        .filter_map(|e| e.get("result").cloned())
        .last()
        .and_then(|r| denial_summary(&r, key))
}

/// Error text for a non-zero agy exit. A permission denial in the stream leads;
/// agy's own stderr follows as secondary detail. Each fragment is redacted
/// before it is capped, and the whole message once more at the end.
fn exit_failure_message(code: i32, stdout: &str, stderr: &str, key: Option<&str>) -> String {
    let msg = match stream_denial_summary(stdout, key) {
        Some(denial) => format!(
            "Antigravity CLI exited with {code}: {denial}; agy also reported: {}",
            redact_then_cap(stderr.trim(), key, 300)
        ),
        None => format!(
            "Antigravity CLI exited with {code}: {}",
            redact_then_cap(stderr, key, 500)
        ),
    };
    setup::redact_key(&msg, key)
}

/// Append why the duduclaw permission rules are missing, when they are.
fn with_grant_note(err: String, grant_issue: Option<&str>) -> String {
    match grant_issue {
        Some(issue) => format!(
            "{err}; DuDuClaw could not add its agy permission rules to \
             ~/.gemini/antigravity-cli/settings.json: {issue}"
        ),
        None => err,
    }
}

/// Hard backstop on the whole subprocess. Kept a notch above `PRINT_TIMEOUT`
/// (agy's own print-mode wait) so agy self-bounds first and this only fires if
/// the process truly wedges.
const DEFAULT_TIMEOUT_SECS: u64 = 330;
/// Value passed to `agy --print-timeout` (agy's CLI default is 5m).
const PRINT_TIMEOUT: &str = "300s";
/// Cap the system prompt embedded into the prompt argument (ARG_MAX safety).
const MAX_SYSTEM_PROMPT_BYTES: usize = 65536;

/// The env the agy spawn hands its MCP child: the identity pair (plus the
/// MCP forward set and this call's home) and, P2-B N4, the turn/run source
/// identity in scope, so the employee's memory writes are tied to their
/// conversation.
fn spawn_mcp_env(home_dir: &std::path::Path, agent_id: &str) -> Vec<(String, String)> {
    let mut env = setup::identity_env_pairs(home_dir, agent_id);
    env.extend(crate::memory_provenance::turn_source_env_pairs());
    env
}

/// Runtime that delegates to the Google Antigravity CLI (`agy`).
pub struct AntigravityRuntime {
    agy_path: String,
    /// Test-only seams for the `#[ignore]` end-to-end tests: a temp user HOME
    /// (settings path + the child's `HOME`), a fixed fake key, and a stand-in
    /// MCP command. Production builds have no such field.
    #[cfg(test)]
    hooks: TestHooks,
}

#[cfg(test)]
#[derive(Default)]
struct TestHooks {
    user_home: Option<std::path::PathBuf>,
    gemini_key: Option<String>,
    mcp_command: Option<(std::path::PathBuf, Vec<String>)>,
}

impl AntigravityRuntime {
    pub fn new() -> Self {
        Self {
            agy_path: resolve_agy_path(),
            #[cfg(test)]
            hooks: TestHooks::default(),
        }
    }

    /// The OS user's home whose `.gemini/antigravity-cli/settings.json` agy reads.
    fn user_home(&self) -> Option<std::path::PathBuf> {
        #[cfg(test)]
        if let Some(h) = &self.hooks.user_home {
            return Some(h.clone());
        }
        dirs::home_dir()
    }

    /// Gemini key for `api_key` mode (rotator → env); see `antigravity_setup`.
    async fn resolve_gemini_key(
        &self,
        context: &RuntimeContext,
    ) -> Option<setup::GeminiKey> {
        #[cfg(test)]
        if let Some(k) = &self.hooks.gemini_key {
            return setup::GeminiKey::new(k.clone());
        }
        setup::resolve_gemini_key(&context.home_dir, &context.account_pool).await
    }

    /// `(command, args)` of the MCP server registered for agy: the absolute
    /// duduclaw binary + `mcp-server`.
    fn mcp_command(&self) -> (std::path::PathBuf, Vec<String>) {
        #[cfg(test)]
        if let Some(c) = &self.hooks.mcp_command {
            return c.clone();
        }
        (
            duduclaw_core::resolve_duduclaw_bin(),
            vec!["mcp-server".to_string()],
        )
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

        // ── Auth route: `config.toml [antigravity] auth` ──────────────────
        // Decided BEFORE anything is written or spawned. In `api_key` mode a
        // missing key fails here with an actionable message instead of agy's
        // generic "authentication required". Absent setting ⇒ no key handling
        // at all (the pre-2026-10 behaviour minus the dead ANTIGRAVITY_API_KEY).
        let auth = setup::auth_mode_from_home(&context.home_dir);
        let candidate_key = if auth == setup::AntigravityAuth::ApiKey {
            self.resolve_gemini_key(context).await
        } else {
            None
        };
        let gemini_key = setup::require_key_for_mode(auth, candidate_key)?;

        // Working root: normally the agent's own directory, but a caller may
        // override the cwd via `super::SPAWN_OVERRIDE` (today: the team
        // composer, putting a role member in the employee's workspace so its
        // files outlive the throwaway scaffold — design §4.3 E3). Identity is
        // NOT affected: it rides this spawn's environment (below), never a
        // file in the shared workspace.
        let work_root: Option<std::path::PathBuf> =
            super::resolve_spawn_work_dir(context.agent_dir.as_deref(), &context.agent_id);

        // User settings, ONE locked read-merge-write:
        //   * trust the working root — agy shows an *interactive* "trust this
        //     workspace?" prompt for any dir not in `trustedWorkspaces`, which
        //     blocks a headless subprocess forever, and an untrusted workspace's
        //     `.agents/mcp_config.json` is not loaded;
        //   * `modelProvider` per the auth mode.
        // Failure is a warning, except in `api_key` mode where agy would
        // silently ignore the key without `modelProvider` — fail closed there.
        let api_key_mode = auth == setup::AntigravityAuth::ApiKey;
        // Set when the duduclaw permission rules could not be written; named in
        // any failure below, since agy will then refuse the MCP tools.
        let mut grant_issue: Option<String> = None;
        match self.user_home() {
            Some(home) => {
                let trusted = work_root.clone();
                let outcome = tokio::task::spawn_blocking(move || {
                    setup::ensure_user_settings(&home, trusted.as_deref(), auth)
                })
                .await;
                let failure = match outcome {
                    Ok(Ok(o)) => {
                        if let Some(ref issue) = o.grant_issue {
                            tracing::warn!(
                                runtime = "antigravity",
                                agent = %context.agent_id,
                                issue = %issue,
                                "could not add the duduclaw permission rules to agy settings.json \
                                 (the operator's `permissions` was left as it is) — agy will \
                                 refuse DuDuClaw MCP tool calls in print mode"
                            );
                            grant_issue = Some(issue.clone());
                        }
                        // C1: a leftover `modelProvider = "gemini"` (from an
                        // earlier `api_key` setting) keeps agy on the key route
                        // even with the setting removed; without a key every
                        // call fails. Say so once; never edit the file here.
                        if auth == setup::AntigravityAuth::Unset
                            && o.had_gemini_provider
                            && duduclaw_core::provider_env::resolve_env_key(setup::GEMINI_PROVIDER)
                                .is_none()
                        {
                            static STALE_WARNED: std::sync::Once = std::sync::Once::new();
                            STALE_WARNED.call_once(|| {
                                tracing::warn!(
                                    runtime = "antigravity",
                                    "agy is still on the Gemini API-key route (modelProvider = \"gemini\" \
                                     in ~/.gemini/antigravity-cli/settings.json, left by an earlier \
                                     [antigravity] auth = \"api_key\") but no GEMINI_API_KEY is set — \
                                     set config.toml [antigravity] auth = \"login\" to switch back to \
                                     Google sign-in, or supply a Gemini key"
                                );
                            });
                        }
                        None
                    }
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(e) => Some(format!("settings task join failed: {e}")),
                };
                if let Some(e) = failure {
                    if api_key_mode {
                        return Err(format!(
                            "Antigravity API key 模式需要在 ~/.gemini/antigravity-cli/settings.json \
                             寫入 \"modelProvider\": \"gemini\"，但寫入失敗：{e}"
                        ));
                    }
                    tracing::warn!(
                        runtime = "antigravity",
                        agent = %context.agent_id,
                        error = %e,
                        "could not update agy settings.json, so the workspace could not be marked \
                         trusted — agy may wait on its interactive trust prompt until the print \
                         timeout; continuing"
                    );
                }
            }
            None if api_key_mode => {
                return Err(
                    "Antigravity API key 模式找不到使用者 HOME，無法設定 agy 的 modelProvider"
                        .to_string(),
                );
            }
            None => {}
        }

        // MCP wiring: register the duduclaw MCP server in the workspace agy
        // actually opens. Idempotent merge; warn-not-fatal — registration
        // failing must not block the reply.
        if let Some(ref root) = work_root {
            if let Err(e) = self.ensure_duduclaw_mcp_config(root, &context.home_dir).await {
                tracing::warn!(
                    runtime = "antigravity",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to write antigravity .agents/mcp_config.json — continuing without it"
                );
            }
        }
        // The pre-2026-10 per-agent settings file was never read by agy and
        // holds a plaintext agent token: remove it when that is all it holds.
        if let Some(ref dir) = context.agent_dir {
            let d = dir.clone();
            match tokio::task::spawn_blocking(move || setup::remove_legacy_agent_settings(&d)).await
            {
                Ok(Ok(true)) => info!(
                    runtime = "antigravity",
                    agent = %context.agent_id,
                    "removed legacy per-agent antigravity settings.json (unused MCP block)"
                ),
                Ok(Ok(false)) => {}
                Ok(Err(e)) => tracing::warn!(
                    runtime = "antigravity",
                    agent = %context.agent_id,
                    error = %e,
                    "failed to remove legacy per-agent antigravity settings.json"
                ),
                Err(e) => tracing::warn!(
                    runtime = "antigravity",
                    agent = %context.agent_id,
                    error = %e,
                    "legacy settings cleanup join failed"
                ),
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
        if let Some(ref dir) = work_root {
            cmd.arg("--add-dir").arg(dir);
            cmd.current_dir(dir);
        }

        // Prompt LAST, as the value of `-p` (see ordering note above).
        cmd.arg("-p").arg(&payload);

        // Identity for the MCP child agy starts (it inherits this env), the MCP
        // forward set and this call's DUDUCLAW_HOME — same as the Grok runtime.
        // N13: agy inherits the gateway's environment; only the scoped
        // upstream-unknown marker (added by `spawn_mcp_env`) may reach the
        // MCP child.
        cmd.env_remove(duduclaw_core::ENV_UPSTREAM_UNKNOWN);
        for (k, v) in spawn_mcp_env(&context.home_dir, &context.agent_id) {
            cmd.env(k, v);
        }
        if let Some((k, v)) = super::round_task_env() {
            cmd.env(k, v);
        }
        match auth {
            setup::AntigravityAuth::ApiKey => {
                // The resolved key is the only Gemini key agy sees.
                for name in duduclaw_core::provider_env::provider_env_key_names(setup::GEMINI_PROVIDER) {
                    cmd.env_remove(name);
                }
                if let Some(ref key) = gemini_key {
                    cmd.env(setup::GEMINI_KEY_ENV, key.expose());
                }
            }
            setup::AntigravityAuth::Login => {
                // Explicit Google sign-in: no key reaches agy.
                for name in duduclaw_core::provider_env::provider_env_key_names(setup::GEMINI_PROVIDER) {
                    cmd.env_remove(name);
                }
            }
            setup::AntigravityAuth::Unset => {}
        }
        #[cfg(test)]
        if let Some(ref h) = self.hooks.user_home {
            // Test-only: point the child at the temp HOME whose settings the
            // runtime just wrote, never the developer's real ~/.gemini, and
            // keep the run hermetic from this shell's DuDuClaw instance.
            cmd.env("HOME", h);
            for var in [
                "DUDUCLAW_MCP_API_KEY",
                "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED",
                "DUDUCLAW_PORT",
                "DUDUCLAW_INSTANCE",
            ] {
                cmd.env_remove(var);
            }
        }
        // Kept in scope until the result is built: every string derived from
        // agy's output is scrubbed of it (and of any Google-key shape).
        let key_for_redaction: Option<&str> = gemini_key.as_ref().map(|k| k.expose());

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
            return Err(with_grant_note(
                exit_failure_message(
                    code,
                    &String::from_utf8_lossy(&output.stdout),
                    &String::from_utf8_lossy(&output.stderr),
                    key_for_redaction,
                ),
                grant_issue.as_deref(),
            ));
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let parsed =
            parse_stream_output_keyed(&raw, key_for_redaction)
                .map_err(|e| with_grant_note(e, grant_issue.as_deref()))?;
        if let Some(reason) = parsed.degraded {
            tracing::warn!(
                runtime = "antigravity",
                agent = %context.agent_id,
                reason = %reason,
                "agy stream did not match the 1.2.10 shape — degraded parse (the reply is \
                 kept; usage may be reported as zero)"
            );
        }
        if let Some(ref denied) = parsed.denied {
            // Tool names only (agy's fixed labels), never tool input.
            tracing::warn!(
                runtime = "antigravity",
                agent = %context.agent_id,
                denied = %denied,
                "agy answered, but refused at least one tool permission in print mode — the \
                 reply may be incomplete"
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
            let stderr = setup::redact_key(&String::from_utf8_lossy(&output.stderr), key_for_redaction);
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
    /// Write MCP server definitions into `<work_root>/.agents/mcp_config.json`,
    /// the workspace file agy loads for a trusted workspace (agy ignores
    /// `mcpServers` inside any `settings.json` — measured on 1.2.14).
    ///
    /// Merges per server name (other servers and unrelated keys are preserved)
    /// under a cross-process lock kept under `duduclaw_home` (never in the
    /// agent-writable workspace) and is idempotent: returns `Ok(false)` without
    /// writing when every requested entry already matches. Symlink-safe: a
    /// symlinked `.agents` or `mcp_config.json` is refused (see
    /// `antigravity_setup::write_mcp_config`). Definitions should carry no
    /// `env` block — identity belongs in the spawn environment.
    pub async fn write_mcp_config(
        work_root: &std::path::Path,
        duduclaw_home: &std::path::Path,
        servers: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<bool, String> {
        let root = work_root.to_path_buf();
        let home = duduclaw_home.to_path_buf();
        let mut list: Vec<(String, serde_json::Value)> = servers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        tokio::task::spawn_blocking(move || setup::write_mcp_config(&root, &home, &list))
            .await
            .map_err(|e| format!("mcp_config write task join failed: {e}"))?
            .map_err(|e| e.to_string())
    }

    /// Ensure the duduclaw MCP server (absolute binary + `mcp-server`, no env)
    /// is registered in `work_root`. Called before every spawn; idempotent.
    async fn ensure_duduclaw_mcp_config(
        &self,
        work_root: &std::path::Path,
        duduclaw_home: &std::path::Path,
    ) -> Result<bool, String> {
        let (command, args) = self.mcp_command();
        let Some(def) = setup::mcp_server_entry(&command, &args) else {
            return Err("duduclaw binary did not resolve to an absolute path".to_string());
        };
        let mut servers = std::collections::HashMap::new();
        servers.insert(setup::MCP_SERVER_NAME.to_string(), def);
        Self::write_mcp_config(work_root, duduclaw_home, &servers).await
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// P2-B N4: the agy spawn env (inherited by its duduclaw MCP child)
    /// carries the turn and run in scope next to the identity pair.
    #[tokio::test]
    async fn spawn_env_carries_the_turn_and_run_in_scope() {
        let home = tempfile::tempdir().unwrap();
        let env = crate::memory_provenance::TURN_USER_MESSAGE
            .scope(Some((12, "2026-10-05T01:02:03Z".into())), async {
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(Some("telegram:4".into()), async {
                        duduclaw_memory::feedback::CURRENT_TURN_ID
                            .scope(Some("t-8".into()), async { spawn_mcp_env(home.path(), "agent-x") })
                            .await
                    })
                    .await
            })
            .await;
        let has = |k: &str, v: &str| env.iter().any(|(a, b)| a == k && b == v);
        assert!(has(duduclaw_core::ENV_AGENT_ID, "agent-x"));
        assert!(has(duduclaw_core::ENV_TRUST_TURN_ID, "t-8"), "{env:?}");
        assert!(has(duduclaw_core::ENV_TRUST_SESSION_ID, "telegram:4"));
        assert!(has(duduclaw_core::ENV_TURN_USER_MESSAGE_SEQ, "12"));
    }

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

    /// Pins what each capability level hands agy today: the argv permission
    /// flags differ by level; the `permissions.allow` rules do not, because
    /// they live in agy's user-level settings file, shared by every agent of
    /// this OS user, so they cannot be set per capability level. They consist
    /// of the duduclaw MCP grant and a read of that server's schema
    /// directory; no command, file-write or URL rule.
    #[test]
    fn capability_levels_map_to_flags_and_only_the_duduclaw_grants() {
        let table: [(Option<CapabilitiesConfig>, &[&str]); 4] = [
            (None, &["--sandbox"]),
            (Some(caps(false, false, &[], &[])), &["--sandbox"]),
            (Some(caps(false, false, &["Read", "Grep"], &[])), &["--sandbox"]),
            (Some(caps(true, false, &[], &[])), &["--dangerously-skip-permissions"]),
        ];
        for (c, want) in table {
            let got = sandbox_args(c.as_ref());
            assert_eq!(got, want.iter().map(|s| s.to_string()).collect::<Vec<_>>());
            assert!(!got.iter().any(|a| a == "--mode"), "no execution-mode flag: {got:?}");
        }
        let home = std::path::Path::new("/u");
        let grants = setup::tool_grants(home);
        let out = setup::merge_user_settings(None, Some("/w"), setup::AntigravityAuth::Unset, &grants)
            .unwrap()
            .content
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let schema_dir = home
            .join(".gemini")
            .join("antigravity-cli")
            .join("mcp")
            .join("duduclaw");
        assert_eq!(
            v["permissions"],
            serde_json::json!({"allow": [
                "mcp(duduclaw/*)",
                format!("read_file({})", schema_dir.to_string_lossy()),
            ]})
        );
    }

    // ── 2026-10: agy's print-mode permission denials must be named ──────────

    /// Observed on agy 1.2.16: a soft-denied write ended the turn, agy then hit
    /// a transient 503 on its retry and reported `status: ERROR` with only the
    /// 503 in `error`; the denial was only in `denied_actions`.
    const DENIED_AFTER_503: &str = concat!(
        "{\"event\":\"step_update\",\"step_update\":{\"step_index\":4,\"step_type\":\"tool\",\"state\":\"DONE\",\"tool_name\":\"call_mcp_tool\",\"tool_info\":{\"parameters\":{\"ServerName\":\"duduclaw\",\"ToolName\":\"ping\"},\"output\":\"TOKEN\"}}}\n",
        "{\"event\":\"result\",\"result\":{\"status\":\"ERROR\",\"response\":\"\",\"error\":\"API error (attempt 1): Error 503, Message: This model is currently experiencing high demand.\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1},\"denied_actions\":[{\"action\":\"write_file\",\"display_name\":\"WriteToFile\"}]}}\n"
    );

    #[test]
    fn a_failed_run_names_the_denied_tool_not_only_the_503() {
        let err = parse_stream_output(DENIED_AFTER_503).unwrap_err();
        assert!(err.contains("WriteToFile"), "{err}");
        assert!(err.contains("denied"), "{err}");
        let denial = err.find("WriteToFile").unwrap();
        if let Some(api) = err.find("503") {
            assert!(denial < api, "the denial must lead, the 503 is secondary: {err}");
        }
    }

    // ── 2026-10 review: redact BEFORE any cut ───────────────────────────────

    /// A key with no Google `AIza` shape: only the exact-match pass of
    /// `redact_key` can catch it, so a cut through it would leak a prefix.
    const CUT_KEY: &str = "dudukey-0123456789abcdefghijklmnopqrstuvwxyz";

    /// No 8-char window of `key` may survive in `text`.
    fn assert_no_key_fragment(text: &str, key: &str) {
        let k: Vec<char> = key.chars().collect();
        for w in k.windows(8) {
            let frag: String = w.iter().collect();
            assert!(!text.contains(&frag), "key fragment {frag:?} leaked in: {text}");
        }
    }

    /// `pad` chars of filler, then the key, so a cap of `cap` chars lands
    /// inside the key.
    fn key_across(cap: usize) -> String {
        format!("{}{CUT_KEY} tail", "x".repeat(cap - 10))
    }

    #[test]
    fn an_exit_failure_never_leaves_a_cut_key_fragment() {
        // Plain path (500-char cap).
        let msg = exit_failure_message(3, "", &key_across(500), Some(CUT_KEY));
        assert_no_key_fragment(&msg, CUT_KEY);
        // Denial path (300-char cap on stderr).
        let msg = exit_failure_message(3, DENIED_AFTER_503, &key_across(300), Some(CUT_KEY));
        assert!(msg.contains("WriteToFile"), "{msg}");
        assert_no_key_fragment(&msg, CUT_KEY);
    }

    #[test]
    fn agys_error_text_never_leaves_a_cut_key_fragment() {
        let result = serde_json::json!({"event": "result", "result": {
            "status": "ERROR", "response": "", "error": key_across(300),
        }});
        let err = parse_stream_output_keyed(&format!("{result}\n"), Some(CUT_KEY)).unwrap_err();
        assert!(err.starts_with("Antigravity result status was ERROR"), "{err}");
        assert_no_key_fragment(&err, CUT_KEY);
    }

    #[test]
    fn a_denial_label_carrying_the_key_is_redacted_before_its_cut() {
        let label = key_across(64);
        let result = serde_json::json!({"event": "result", "result": {
            "status": "SUCCESS", "response": "",
            "denied_actions": [{"action": "command", "display_name": label}],
        }});
        let err = parse_stream_output_keyed(&format!("{result}\n"), Some(CUT_KEY)).unwrap_err();
        assert_no_key_fragment(&err, CUT_KEY);
    }

    #[test]
    fn a_failure_names_a_skipped_permission_grant() {
        let e = with_grant_note("boom".into(), Some("non-object permissions"));
        assert!(e.starts_with("boom; DuDuClaw could not add its agy permission rules"), "{e}");
        assert!(e.ends_with("non-object permissions"), "{e}");
        assert_eq!(with_grant_note("boom".into(), None), "boom");
    }

    #[test]
    fn a_failed_run_without_a_denial_carries_agys_error_text() {
        let raw = "{\"event\":\"result\",\"result\":{\"status\":\"ERROR\",\"response\":\"\",\"error\":\"API error (attempt 1): Error 503\"}}\n";
        let err = parse_stream_output(raw).unwrap_err();
        assert!(err.starts_with("Antigravity result status was ERROR, not SUCCESS: "), "{err}");
        assert!(err.contains("Error 503"), "{err}");
    }

    #[test]
    fn a_success_status_with_no_answer_and_a_denial_is_an_error_naming_it() {
        // Observed on agy 1.2.16 (RunCommand soft-denied): status SUCCESS,
        // empty response. The old fallback returned the raw result JSON line
        // as the employee's answer.
        let raw = "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1},\"denied_actions\":[{\"action\":\"command\",\"display_name\":\"RunCommand\"}]}}\n";
        let err = parse_stream_output(raw).unwrap_err();
        assert!(err.contains("RunCommand"), "{err}");
        assert!(!err.contains("{\"event\""), "raw stream JSON leaked into the error: {err}");
    }

    #[test]
    fn a_reply_that_also_saw_a_denial_is_kept() {
        let raw = "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"done\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1},\"denied_actions\":[{\"action\":\"command\",\"display_name\":\"RunCommand\"}]}}\n";
        let parsed = parse_stream_output(raw).unwrap();
        assert_eq!(parsed.content, "done");
        // Reported for the caller's warn!, tool name only.
        let denied = parsed.denied.expect("denial reported");
        assert!(denied.contains("RunCommand"), "{denied}");
        // A clean run reports none.
        let clean = "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"done\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n";
        assert!(parse_stream_output(clean).unwrap().denied.is_none());
    }

    #[test]
    fn a_non_zero_exit_leads_with_the_denied_tool() {
        let stderr = "error: API error (attempt 1): Error 503, Message: high demand\n";
        let msg = exit_failure_message(3, DENIED_AFTER_503, stderr, None);
        assert!(msg.starts_with("Antigravity CLI exited with 3: "), "{msg}");
        let denial = msg.find("WriteToFile").expect("denial named");
        let api = msg.find("503").expect("agy's own error kept");
        assert!(denial < api, "{msg}");
        // No denial in the stream ⇒ the old shape (stderr only).
        let plain = exit_failure_message(3, "", stderr, None);
        assert_eq!(plain, format!("Antigravity CLI exited with 3: {stderr}"));
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

    // ── 2026-10 key route + MCP registration, against the REAL agy ──────────
    //
    // Both tests point the agy child's HOME at a temp directory (test-only hook;
    // production never overrides HOME), so the developer's real `~/.gemini` and
    // Google sign-in are never read or written, and use a FAKE key, so no model
    // call can succeed. Run with:
    //   cargo test -p duduclaw-gateway --lib --no-default-features -- --ignored \
    //     e2e_agy_api_key_mode_reaches_gemini_with_the_runtime_settings \
    //     e2e_agy_starts_the_registered_mcp_server_with_the_agent_identity

    const FAKE_KEY: &str = concat!("AI", "zaSyDUDUCLAW-e2e-fake-key-000000000000");

    fn installed_agy() -> Option<String> {
        let p = resolve_agy_path();
        if p != "agy" {
            return Some(p);
        }
        duduclaw_core::which_agy()
    }

    fn installed_python3() -> Option<std::path::PathBuf> {
        ["/opt/homebrew/bin/python3", "/usr/local/bin/python3", "/usr/bin/python3"]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.is_file())
    }

    struct E2eDirs {
        _tmp: tempfile::TempDir,
        user_home: std::path::PathBuf,
        duduclaw_home: std::path::PathBuf,
        agent_dir: std::path::PathBuf,
    }

    fn e2e_dirs() -> E2eDirs {
        let tmp = tempfile::tempdir().unwrap();
        let user_home = tmp.path().join("user-home");
        let duduclaw_home = tmp.path().join("duduclaw-home");
        let agent_dir = tmp.path().join("agent");
        for d in [&user_home, &duduclaw_home, &agent_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            duduclaw_home.join("config.toml"),
            "[antigravity]\nauth = \"api_key\"\n",
        )
        .unwrap();
        E2eDirs {
            _tmp: tmp,
            user_home,
            duduclaw_home,
            agent_dir,
        }
    }

    fn e2e_runtime(
        agy: String,
        dirs: &E2eDirs,
        mcp_command: (std::path::PathBuf, Vec<String>),
    ) -> AntigravityRuntime {
        AntigravityRuntime {
            agy_path: agy,
            hooks: TestHooks {
                user_home: Some(dirs.user_home.clone()),
                gemini_key: Some(FAKE_KEY.to_string()),
                mcp_command: Some(mcp_command),
            },
        }
    }

    fn e2e_ctx(dirs: &E2eDirs, agent_id: &str) -> RuntimeContext {
        RuntimeContext {
            agent_dir: Some(dirs.agent_dir.clone()),
            system_prompt: String::new(),
            model: String::new(),
            max_tokens: 64,
            home_dir: dirs.duduclaw_home.clone(),
            agent_id: agent_id.to_string(),
            preferred_provider: None,
            conversation_history: vec![],
            capabilities: None,
            account_pool: vec![],
            effort: None,
            allow_cross_family_failover: false,
        }
    }

    #[tokio::test]
    #[ignore = "drives the real agy binary (temp HOME, fake key, zero inference)"]
    async fn e2e_agy_api_key_mode_reaches_gemini_with_the_runtime_settings() {
        let Some(agy) = installed_agy() else {
            eprintln!("agy not installed — skipping");
            return;
        };
        let dirs = e2e_dirs();
        let rt = e2e_runtime(
            agy,
            &dirs,
            (std::path::PathBuf::from("/usr/bin/true"), vec![]),
        );
        let err = rt
            .execute("Reply with exactly: PONG", &e2e_ctx(&dirs, "e2e-agy-key"))
            .await
            .expect_err("a fake key must never produce a reply");
        eprintln!("agy failure text: {err}");
        assert!(
            err.contains("API key not valid"),
            "the Gemini key route was not engaged: {err}"
        );
        assert!(!err.contains(FAKE_KEY), "the key must not be echoed: {err}");

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(setup::user_settings_path(&dirs.user_home)).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["modelProvider"], "gemini");
        let canon = dirs.agent_dir.canonicalize().unwrap();
        assert_eq!(
            settings["trustedWorkspaces"],
            serde_json::json!([canon.to_string_lossy()])
        );
    }

    #[tokio::test]
    #[ignore = "drives the real agy binary (temp HOME, fake key, stub MCP server)"]
    async fn e2e_agy_starts_the_registered_mcp_server_with_the_agent_identity() {
        let Some(agy) = installed_agy() else {
            eprintln!("agy not installed — skipping");
            return;
        };
        let Some(python) = installed_python3() else {
            eprintln!("python3 not installed — skipping");
            return;
        };
        let dirs = e2e_dirs();
        let marker = dirs.user_home.join("mcp-started.txt");
        let script = dirs.user_home.join("stub_mcp.py");
        std::fs::write(
            &script,
            format!(
                r#"import json, os, sys
with open({marker:?}, "a") as f:
    f.write(os.environ.get("DUDUCLAW_AGENT_ID", "<none>") + "\n")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {{"protocolVersion": msg.get("params", {{}}).get("protocolVersion", "2024-11-05"),
                  "capabilities": {{"tools": {{}}}},
                  "serverInfo": {{"name": "stub", "version": "0"}}}}
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": result}}
    elif method == "tools/list":
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"tools": []}}}}
    else:
        out = {{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -32601, "message": "no"}}}}
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
"#,
                marker = marker.to_string_lossy()
            ),
        )
        .unwrap();
        // A pre-2026-10 per-agent file holding only the plaintext MCP block.
        let legacy = setup::legacy_agent_settings_path(&dirs.agent_dir);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            r#"{"mcpServers":{"duduclaw":{"env":{"DUDUCLAW_AGENT_TOKEN":"stale"}}}}"#,
        )
        .unwrap();

        let rt = e2e_runtime(
            agy,
            &dirs,
            (python, vec![script.to_string_lossy().into_owned()]),
        );
        let res = rt
            .execute("Reply with exactly: PONG", &e2e_ctx(&dirs, "e2e-agy-mcp"))
            .await;
        let err = res.expect_err("a fake key must never produce a reply");
        eprintln!("agy failure text: {err}");

        let cfg = std::fs::read_to_string(setup::mcp_config_path(&dirs.agent_dir)).unwrap();
        assert!(!cfg.contains("DUDUCLAW_AGENT"), "identity must not be on disk: {cfg}");
        assert!(!legacy.exists(), "the mcp-only legacy file must be deleted");

        let started = std::fs::read_to_string(&marker).unwrap_or_default();
        assert!(
            started.lines().any(|l| l == "e2e-agy-mcp"),
            "stub MCP server was not started with the agent id; marker: {started:?}"
        );
    }

    // ── 2026-10 key route with a REAL Gemini API key (paid inference) ───────
    //
    // The removal precondition for the Gemini CLI runtime
    // (docs/guides/deprecations.md): agy's `api_key` mode must answer with a
    // real key. Same temp-HOME isolation as the two tests above — the
    // developer's `~/.gemini` is never read or written. The key comes ONLY from
    // `DUDUCLAW_AGY_REAL_GEMINI_KEY`; unset ⇒ the tests return without calling
    // anything. Each test makes exactly one model call. Run with:
    //   DUDUCLAW_AGY_REAL_GEMINI_KEY="$(cat <key file>)" cargo test -p duduclaw-gateway \
    //     --lib e2e_agy_api_key_mode_real_key -- --ignored --nocapture --test-threads=1
    // Optional: `DUDUCLAW_AGY_REAL_MODEL="<display name>"` picks the model for
    // the named-model case (default: the first "Flash (Low)" entry `agy models`
    // lists in key mode, else the catalog fallback name).

    const REAL_KEY_ENV: &str = "DUDUCLAW_AGY_REAL_GEMINI_KEY";
    /// Catalog fallback display name (`runtime_catalog.rs`), used only when
    /// `agy models` lists nothing in key mode.
    const CATALOG_FALLBACK_MODEL: &str = "Gemini 3.5 Flash (Medium)";

    fn real_key() -> Option<String> {
        match std::env::var(REAL_KEY_ENV) {
            Ok(k) if !k.trim().is_empty() => Some(k.trim().to_string()),
            _ => {
                eprintln!(
                    "{REAL_KEY_ENV} not set — skipping (this test spends one real Gemini call)"
                );
                None
            }
        }
    }

    fn write_stub_mcp(dir: &std::path::Path, marker: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("stub_mcp.py");
        std::fs::write(
            &script,
            format!(
                r#"import json, os, sys
with open({marker:?}, "a") as f:
    f.write(os.environ.get("DUDUCLAW_AGENT_ID", "<none>") + "\n")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {{"protocolVersion": msg.get("params", {{}}).get("protocolVersion", "2024-11-05"),
                  "capabilities": {{"tools": {{}}}},
                  "serverInfo": {{"name": "stub", "version": "0"}}}}
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": result}}
    elif method == "tools/list":
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"tools": []}}}}
    else:
        out = {{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -32601, "message": "no"}}}}
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
"#,
                marker = marker.to_string_lossy()
            ),
        )
        .unwrap();
        script
    }

    /// `agy models` under the temp HOME in key mode (a listing call, no
    /// inference). Returns `(id, display name)` rows.
    fn list_models_in_key_mode(agy: &str, user_home: &std::path::Path, key: &str) -> Vec<(String, String)> {
        let settings = setup::user_settings_path(user_home);
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        if !settings.exists() {
            std::fs::write(&settings, r#"{"modelProvider":"gemini"}"#).unwrap();
        }
        let out = std::process::Command::new(agy)
            .arg("models")
            .env("HOME", user_home)
            .env("GEMINI_API_KEY", key)
            .env_remove("GOOGLE_API_KEY")
            .current_dir(user_home)
            .output();
        // Leave no settings behind: the runtime must write `modelProvider` itself.
        let _ = std::fs::remove_file(&settings);
        let Ok(out) = out else { return Vec::new() };
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.contains(key), "agy models echoed the key");
        text.lines()
            .filter_map(|l| {
                let (id, name) = l.split_once('\t')?;
                Some((id.trim().to_string(), name.trim().to_string()))
            })
            .collect()
    }

    /// Print the model-selection lines of agy's own log under the temp HOME
    /// (key-scrubbed), so the run shows which model agy actually used.
    fn print_agy_model_log_lines(user_home: &std::path::Path, key: &str) {
        let log_dir = user_home.join(".gemini").join("antigravity-cli").join("log");
        let Ok(rd) = std::fs::read_dir(&log_dir) else {
            eprintln!("[model-evidence] no agy log dir");
            return;
        };
        for entry in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            assert!(!text.contains(key), "agy log contains the key");
            for line in text.lines() {
                let lower = line.to_ascii_lowercase();
                if lower.contains("model") || lower.contains("gemini-") {
                    let shown = setup::redact_key(line, Some(key));
                    eprintln!("[model-evidence] {}", duduclaw_core::truncate_bytes(&shown, 400));
                }
            }
        }
    }

    async fn run_real_key_case(model: Option<String>, agent_id: &str) {
        let Some(key) = real_key() else { return };
        let Some(agy) = installed_agy() else {
            eprintln!("agy not installed — skipping");
            return;
        };
        let Some(python) = installed_python3() else {
            eprintln!("python3 not installed — skipping");
            return;
        };
        let dirs = e2e_dirs();
        let marker = dirs.user_home.join("mcp-started.txt");
        let script = write_stub_mcp(&dirs.user_home, &marker);

        let model = match model {
            Some(m) => {
                let listed = list_models_in_key_mode(&agy, &dirs.user_home, &key);
                eprintln!("[models] agy models in key mode listed {} entries:", listed.len());
                for (id, name) in &listed {
                    eprintln!("[models]   {id}\t{name}");
                }
                let chosen = if !m.is_empty() {
                    m
                } else if let Some((_, name)) = listed
                    .iter()
                    .find(|(_, n)| n.contains("Flash") && n.contains("(Low)"))
                {
                    name.clone()
                } else if let Some((_, name)) = listed.first() {
                    name.clone()
                } else {
                    CATALOG_FALLBACK_MODEL.to_string()
                };
                eprintln!("[models] using display name: {chosen:?}");
                chosen
            }
            None => String::new(),
        };

        let mut rt = e2e_runtime(
            agy,
            &dirs,
            (python, vec![script.to_string_lossy().into_owned()]),
        );
        rt.hooks.gemini_key = Some(key.clone());
        let mut ctx = e2e_ctx(&dirs, agent_id);
        ctx.model = model.clone();

        let res = rt.execute("Reply with exactly: PONG", &ctx).await;
        print_agy_model_log_lines(&dirs.user_home, &key);
        let resp = match res {
            Ok(r) => r,
            Err(e) => {
                assert!(!e.contains(&key), "the error text must not carry the key");
                panic!("real-key agy call failed (model {model:?}): {e}");
            }
        };
        eprintln!(
            "agy replied (model {model:?}): {:?}; tokens in/out/cache = {}/{}/{}",
            resp.content, resp.input_tokens, resp.output_tokens, resp.cache_read_tokens
        );
        assert!(!resp.content.contains(&key), "the reply must not carry the key");
        assert!(!resp.content.trim().is_empty(), "empty reply");
        assert!(resp.content.contains("PONG"), "reply lacks PONG: {:?}", resp.content);
        assert_eq!(resp.runtime_name, "antigravity");

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(setup::user_settings_path(&dirs.user_home)).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["modelProvider"], "gemini");

        let started = std::fs::read_to_string(&marker).unwrap_or_default();
        assert!(
            started.lines().any(|l| l == agent_id),
            "stub MCP server was not started with the agent id; marker: {started:?}"
        );
    }

    #[tokio::test]
    #[ignore = "spends one real Gemini API call; needs DUDUCLAW_AGY_REAL_GEMINI_KEY"]
    async fn e2e_agy_api_key_mode_real_key_replies_default_model() {
        run_real_key_case(None, "e2e-agy-real-default").await;
    }

    /// A stub MCP server with ONE tool, `ping`, which returns `token` and
    /// appends `called <tool> agent=<DUDUCLAW_AGENT_ID>` to `log`.
    fn write_stub_mcp_with_tool(
        dir: &std::path::Path,
        log: &std::path::Path,
        token: &str,
    ) -> std::path::PathBuf {
        let script = dir.join("stub_mcp_tool.py");
        std::fs::write(
            &script,
            format!(
                r#"import json, os, sys
LOG = {log:?}
AGENT = os.environ.get("DUDUCLAW_AGENT_ID", "<none>")
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if "id" not in msg:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {{"protocolVersion": msg.get("params", {{}}).get("protocolVersion", "2024-11-05"),
                  "capabilities": {{"tools": {{}}}},
                  "serverInfo": {{"name": "stub", "version": "0"}}}}
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": result}}
    elif method == "tools/list":
        tool = {{"name": "ping", "description": "Returns a token.",
                "inputSchema": {{"type": "object", "properties": {{}}}}}}
        out = {{"jsonrpc": "2.0", "id": msg["id"], "result": {{"tools": [tool]}}}}
    elif method == "tools/call":
        with open(LOG, "a") as f:
            f.write("called " + str(msg.get("params", {{}}).get("name")) + " agent=" + AGENT + "\n")
        out = {{"jsonrpc": "2.0", "id": msg["id"],
               "result": {{"content": [{{"type": "text", "text": {token:?}}}]}}}}
    else:
        out = {{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -32601, "message": "no"}}}}
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()
"#,
                log = log.to_string_lossy(),
            ),
        )
        .unwrap();
        script
    }

    /// The 2026-10 defect: with the default capability level (WorkspaceWrite ⇒
    /// `--sandbox`) agy's print mode soft-denied every MCP tool call. The model
    /// must actually call the duduclaw server's tool and get its result back.
    #[tokio::test]
    #[ignore = "spends one real Gemini API call; needs DUDUCLAW_AGY_REAL_GEMINI_KEY"]
    async fn e2e_agy_real_key_default_level_calls_a_duduclaw_mcp_tool() {
        let Some(key) = real_key() else { return };
        let Some(agy) = installed_agy() else {
            eprintln!("agy not installed — skipping");
            return;
        };
        let Some(python) = installed_python3() else {
            eprintln!("python3 not installed — skipping");
            return;
        };
        let dirs = e2e_dirs();
        let log = dirs.user_home.join("mcp-calls.txt");
        let token = "TOKEN-K7Q4Z";
        let script = write_stub_mcp_with_tool(&dirs.user_home, &log, token);

        let model = match std::env::var("DUDUCLAW_AGY_REAL_MODEL") {
            Ok(m) if !m.trim().is_empty() => m.trim().to_string(),
            _ => list_models_in_key_mode(&agy, &dirs.user_home, &key)
                .into_iter()
                .map(|(_, name)| name)
                .find(|n| n.contains("Flash") && n.contains("(Low)"))
                .unwrap_or_else(|| CATALOG_FALLBACK_MODEL.to_string()),
        };
        eprintln!("[models] using display name: {model:?}");

        let mut rt = e2e_runtime(
            agy,
            &dirs,
            (python, vec![script.to_string_lossy().into_owned()]),
        );
        rt.hooks.gemini_key = Some(key.clone());
        let agent_id = "e2e-agy-real-tool";
        let mut ctx = e2e_ctx(&dirs, agent_id);
        ctx.model = model.clone();
        assert!(ctx.capabilities.is_none(), "the default level is under test");
        assert_eq!(sandbox_args(ctx.capabilities.as_ref()), vec!["--sandbox"]);

        let res = rt
            .execute(
                "Call the ping tool of the duduclaw MCP server, then reply with exactly the text it returned.",
                &ctx,
            )
            .await;
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        eprintln!("[mcp] stub call log: {calls:?}");
        let resp = match res {
            Ok(r) => r,
            Err(e) => {
                assert!(!e.contains(&key), "the error text must not carry the key");
                panic!("real-key agy tool call failed (model {model:?}): {e}");
            }
        };
        eprintln!(
            "agy replied (model {model:?}): {:?}; tokens in/out = {}/{}",
            resp.content, resp.input_tokens, resp.output_tokens
        );
        assert!(!resp.content.contains(&key), "the reply must not carry the key");
        assert!(
            calls.lines().any(|l| l == format!("called ping agent={agent_id}")),
            "the stub tool was not called with the agent identity: {calls:?}"
        );
        assert!(resp.content.contains(token), "reply lacks the tool result: {:?}", resp.content);
    }

    #[tokio::test]
    #[ignore = "spends one real Gemini API call; needs DUDUCLAW_AGY_REAL_GEMINI_KEY"]
    async fn e2e_agy_api_key_mode_real_key_replies_with_display_name() {
        let pinned = std::env::var("DUDUCLAW_AGY_REAL_MODEL").unwrap_or_default();
        run_real_key_case(Some(pinned), "e2e-agy-real-named").await;
    }
}
