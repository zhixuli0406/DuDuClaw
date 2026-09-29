use super::*;

/// Keepalive interval — send progress if no stream-json events for this long.
pub(crate) const KEEPALIVE_INTERVAL_SECS: u64 = 90;

/// Hard max timeout — absolute safety net to kill truly hung processes.
pub(super) const HARD_MAX_TIMEOUT_SECS: u64 = 30 * 60; // 30 minutes

/// Internal wrapper for GVU loop / internal-utility LLM calls.
///
/// RFC-25 Phase 0: the previous hard allowlist *rejected* any model that wasn't
/// `claude-haiku-4-5`, which made evolution/internal tasks impossible to run on
/// a different (even Claude-family) model and blocked the multi-runtime goal.
/// We now warn on unrecognised evolution models instead of failing — the agent's
/// configured `[model] utility` is honoured. Provider-level routing (Codex/Gemini)
/// arrives via the choke-point in Phase 1-2.
pub(super) const KNOWN_EVOLUTION_MODELS: &[&str] = &["claude-haiku-4-5", "claude-haiku-4-5-20250307"];

pub(crate) async fn call_claude_cli_public(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
) -> Result<String, String> {
    if !KNOWN_EVOLUTION_MODELS.contains(&model) {
        warn!(
            model,
            "call_claude_cli_public: non-default evolution/utility model — proceeding (RFC-25 Phase 0)"
        );
    }
    // Use account-rotated path so GVU benefits from multi-account failover
    // instead of failing silently when the ambient account is rate-limited.
    call_claude_cli_rotated(
        user_message,
        model,
        system_prompt,
        home_dir,
        None,
        None,
        None,
        None,
        &[],
        // GVU / internal-utility call — not an agent turn, so no account pool.
        &[],
        None,
    )
    .await
}

/// Call the `claude` CLI (Claude Code SDK) with streaming output.
///
/// Uses `--output-format stream-json --verbose` to read incremental events.
/// Instead of killing on idle, sends keepalive progress to the channel via
/// `on_progress` callback. A hard max timeout (30 min) acts as safety net.
///
/// Thin wrapper around [`spawn_claude_cli_with_env`] that uses the ambient
/// environment (and any configured `ANTHROPIC_API_KEY` as fallback). This is
/// the no-rotation path — used by compression and GVU reflection helpers.
/// The main channel-reply path goes through [`call_claude_cli_rotated`].
pub(super) async fn call_claude_cli(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    work_dir: Option<&Path>,
    on_progress: Option<&ProgressCallback>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
) -> Result<String, String> {
    let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    spawn_claude_cli_with_env(
        user_message,
        model,
        system_prompt,
        home_dir,
        work_dir,
        on_progress,
        capabilities,
        &empty,
        None,
        None,
    )
    .await
}

/// Lightweight Claude CLI call for single-turn metadata tasks.
///
/// Optimized for: session compression, instruction extraction, key-fact extraction,
/// GVU evolution, wiki ingest. Uses `--bare --effort medium --max-turns 1
/// --no-session-persistence --tools ""` for minimal overhead and cost.
///
/// Estimated 25-40% cost reduction vs the full channel reply path.
pub(super) async fn call_claude_cli_lightweight(
    prompt: &str,
    model: &str,
    home_dir: &Path,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let claude_path =
        duduclaw_core::which_claude().ok_or_else(|| "claude CLI not found in PATH".to_string())?;

    let api_key = get_api_key(home_dir).await;

    let mut cmd = duduclaw_core::platform::async_command_for(&claude_path);
    cmd.args([
        // NOTE: `--bare` removed — Claude CLI 2.1.110 regresses OAuth auth when
        // the flag is active (kills keychain lookup alongside the hook/LSP skips).
        // Lightweight path still relies on --max-turns 1 + --no-session-persistence
        // + --tools "" to keep the call cheap.
        // P1/WP-3: this is the ONE pre-existing `--effort` call site in the
        // repo, and it stays a hardcoded constant ON PURPOSE. The lightweight
        // path serves session compression / GVU / wiki ingest — mechanical
        // extraction whose cost profile must not move when an operator raises a
        // conversational agent's `[model] effort` to `max`. Deliberately NOT
        // threaded from `duduclaw_core::effort`; do not "fix" this to read the
        // agent's setting.
        "--effort",
        "medium", // Balanced: no full thinking but adequate extraction quality
        "--max-turns",
        "1",                        // Single-turn only (no tool use)
        "--no-session-persistence", // Throwaway call, don't save session
        "--tools",
        "", // Disable all built-in tools (pure text response)
        "-p",
        prompt,
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--verbose",
        "--dangerously-skip-permissions",
    ]);

    if let Some(ref key) = api_key {
        cmd.env("ANTHROPIC_API_KEY", key);
    }
    cmd.env_remove("CLAUDECODE");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("claude CLI spawn error: {e}"))?;
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    let mut result_text = String::new();
    while let Ok(Some(line)) = reader.next_line().await {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                if !text.is_empty() {
                    result_text = text.to_string();
                }
            }
            if event.get("type").and_then(|t| t.as_str()) == Some("assistant") {
                if let Some(content) = event
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                if !t.is_empty() {
                                    result_text = t.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let status = child.wait().await.map_err(|e| format!("wait error: {e}"))?;
    if !status.success() && result_text.is_empty() {
        return Err(format!("claude CLI exited with {status}"));
    }

    if result_text.is_empty() {
        Err("Empty response from lightweight CLI call".to_string())
    } else {
        Ok(result_text)
    }
}

/// Try the `claude` CLI with rotation across configured `AccountRotator` accounts.
///
/// On each attempt the rotator selects an account and yields its env vars
/// (`CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CONFIG_DIR`, or `ANTHROPIC_API_KEY`).
/// Classifies failures and feeds them back to the rotator so unhealthy
/// accounts cool down correctly. Falls through to the non-rotated path
/// when no accounts are configured (fresh-install passthrough).
/// `Some(zh-TW error)` when a `moa:` virtual-model id reaches a CLI-spawn
/// path. MoA ensembles execute through the API-mode executor only
/// (`direct_api::call_moa_model`); passing the id to `claude -p` would
/// produce a confusing upstream model-not-found error.
pub(crate) fn reject_moa_on_cli_path(model: &str) -> Option<String> {
    if duduclaw_llm::is_moa_model_id(model) {
        Some(format!(
            "MoA 模型 `{model}` 僅支援 API 模式，無法經由 Claude CLI 執行。\
             請確認帳號池中有各成員 provider 的 API key（或設定對應環境變數）。"
        ))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// S2 provenance policy (config.toml [provenance]) — tool-loop taint tracking
// ---------------------------------------------------------------------------

/// Wiki-read tools whose *results* are trusted (curated, scope-policed,
/// citation-tracked content — see the v1.33 wiki ↔ memory boundary).
pub(super) const PROVENANCE_TRUSTED_WIKI_TOOLS: &[&str] = &["shared_wiki_read", "shared_wiki_search"];

/// Parse `config.toml [provenance]` into `(policy, sensitive tool names)`.
///
/// Shape:
/// ```toml
/// [provenance]
/// policy = "off" | "warn" | "enforce"   # default (and any unknown value): off
/// sensitive_tools = ["send_to_agent", "shared_wiki_write"]
/// ```
/// Absent section / malformed values ⇒ `(Off, [])` — byte-identical loop
/// behavior to pre-S2 (the library skips every provenance branch under Off).
pub fn parse_provenance_settings(
    config: &toml::Table,
) -> (duduclaw_llm::ProvenancePolicy, Vec<String>) {
    use duduclaw_llm::ProvenancePolicy;
    let Some(section) = config.get("provenance").and_then(|v| v.as_table()) else {
        return (ProvenancePolicy::Off, Vec::new());
    };
    let policy = match section.get("policy").and_then(|v| v.as_str()) {
        Some("warn") => ProvenancePolicy::Warn,
        Some("enforce") => ProvenancePolicy::Enforce,
        Some("off") | None => ProvenancePolicy::Off,
        Some(other) => {
            warn!(
                policy = other,
                "[provenance] unknown policy value — treating as \"off\" (valid: off|warn|enforce)"
            );
            ProvenancePolicy::Off
        }
    };
    let sensitive_tools = section
        .get("sensitive_tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    (policy, sensitive_tools)
}

/// Build the [`duduclaw_llm::ProvenanceConfig`] for one channel turn.
///
/// - `policy == Off` ⇒ `ProvenanceConfig::default()` — the tool loop is
///   byte-identical to pre-S2 (no ledger, no checks).
/// - Otherwise: the channel user input is seeded **Tainted**
///   ([`duduclaw_llm::SourceKind::ChannelUserInput`]) on the initial ledger,
///   the listed sensitive tools are gated on all args, and the wiki-read
///   tools' results are declared **Trusted** ([`duduclaw_llm::SourceKind::Wiki`]).
pub fn build_channel_provenance_config(
    policy: duduclaw_llm::ProvenancePolicy,
    sensitive_tools: &[String],
    channel_user_input: &str,
) -> duduclaw_llm::ProvenanceConfig {
    use duduclaw_llm::{
        ProvenanceConfig, ProvenanceLedger, ProvenancePolicy, SensitiveTool, SourceKind,
    };
    if policy == ProvenancePolicy::Off {
        return ProvenanceConfig::default();
    }
    let mut ledger = ProvenanceLedger::new();
    ledger.register(channel_user_input, SourceKind::ChannelUserInput);
    let tool_trust = PROVENANCE_TRUSTED_WIKI_TOOLS
        .iter()
        .map(|t| (t.to_string(), SourceKind::Wiki))
        .collect();
    ProvenanceConfig {
        policy,
        sensitive_tools: sensitive_tools
            .iter()
            .map(|n| SensitiveTool::all_args(n.clone()))
            .collect(),
        tool_trust,
        initial_ledger: Some(ledger),
    }
}

#[allow(clippy::too_many_arguments)] // one extra pass-through param (account_pool)
pub(crate) async fn call_claude_cli_rotated(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    work_dir: Option<&Path>,
    on_progress: Option<&ProgressCallback>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    // `_session_id` retained in the signature for call-site compatibility;
    // the Claude CLI `--resume` path was removed (see module note above
    // `rotate_cli_spawn` invocation). History is folded into the prompt
    // instead.
    _session_id: Option<&str>,
    conversation_history: &[ConversationTurn],
    // The answering agent's `agent.toml [model] account_pool`. Empty (`&[]`)
    // for agent-less system callers (dashboard widget / expert-pack
    // generation) — behavior is then byte-identical to before the pool
    // existed.
    account_pool: &[String],
    // P1/WP-3: per-call reasoning effort (`agent.toml [model] effort`, or a
    // team role spec). `None` ⇒ no `--effort` flag, argv byte-identical.
    effort: Option<duduclaw_core::effort::Effort>,
) -> Result<String, String> {
    // MoA virtual models must never reach a CLI spawn — fail with a clear
    // reason instead of a confusing upstream model-not-found error.
    if let Some(msg) = reject_moa_on_cli_path(model) {
        return Err(msg);
    }
    // P1/WP-3: an explicit per-call effort (a team role spec, the multi-runtime
    // choke-point) wins; otherwise fall back to the answering agent's own
    // `agent.toml [model] effort`. `work_dir` is the agent directory. Both
    // absent ⇒ `None` and the argv is byte-identical.
    let effort = effort.or_else(|| work_dir.and_then(duduclaw_core::effort::read_agent_effort));
    let rotator = match crate::claude_runner::get_rotator_cached(home_dir).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "Rotator unavailable — falling back to non-rotated CLI path");
            // Fallback: prepend history to prompt for non-rotated path
            let effective_msg = if conversation_history.is_empty() {
                user_message.to_string()
            } else {
                format_history_as_prompt(conversation_history, user_message)
            };
            return call_claude_cli(
                &effective_msg,
                model,
                system_prompt,
                home_dir,
                work_dir,
                on_progress,
                capabilities,
            )
            .await;
        }
    };

    let account_count = rotator.count().await;
    if account_count == 0 {
        // Fresh install — no accounts configured. Use ambient env.
        let effective_msg = if conversation_history.is_empty() {
            user_message.to_string()
        } else {
            format_history_as_prompt(conversation_history, user_message)
        };
        return call_claude_cli(
            &effective_msg,
            model,
            system_prompt,
            home_dir,
            work_dir,
            on_progress,
            capabilities,
        )
        .await;
    }

    // Delegate to the testable primitive with a closure that actually spawns the CLI.
    //
    // Claude CLI `-p --resume <id>` only accepts either a canonical UUID (that
    // already exists in its session store) or an exact session title match, so
    // DuDuClaw's deterministic `dd-<hash>` IDs were rejected 100% of the time
    // and every multi-turn wasted one CLI spawn before falling back to
    // history-in-prompt. We skip `--resume` entirely and always fold the
    // conversation history into the prompt when there is any — one spawn per
    // turn, no log noise, no cost duplication.
    let input_len = user_message.len();
    let history_clone = conversation_history.to_vec();
    rotate_cli_spawn(
        &rotator,
        account_pool,
        move |env_vars, retry_hint| {
            let model = model.to_string();
            let system_prompt = system_prompt.to_string();
            let home_dir = home_dir.to_path_buf();
            let work_dir = work_dir.map(|p| p.to_path_buf());
            let on_progress = on_progress;
            let capabilities = capabilities.cloned();
            let history = history_clone.clone();
            let user_message_owned = user_message.to_string();
            let effort = effort;
            async move {
                let mut effective_prompt = if history.is_empty() {
                    user_message_owned
                } else {
                    format_history_as_prompt(&history, &user_message_owned)
                };
                // Summarized-failure retry: one-line hint appended to the user
                // message (never the system prompt — keeps its cache prefix stable).
                if let Some(hint) = retry_hint {
                    effective_prompt =
                        format!("{effective_prompt}\n\n<retry_context>{hint}</retry_context>");
                }
                spawn_claude_cli_with_env(
                    &effective_prompt,
                    &model,
                    &system_prompt,
                    &home_dir,
                    work_dir.as_deref(),
                    on_progress,
                    capabilities.as_ref(),
                    &env_vars,
                    None,
                    effort,
                )
                .await
            }
        },
        input_len,
    )
    .await
}

/// Rotation-loop primitive, decoupled from the actual subprocess spawn.
///
/// Iterates `rotator.select()` up to `rotator.count()` times. For each
/// selected account, calls the provided `spawn` closure with the env-var
/// map and an optional retry hint. On success, records cost telemetry and
/// returns. On failure, classifies the error and feeds it back to the
/// rotator (`on_billing_exhausted`, `on_rate_limited`, or `on_error`).
/// Returns the last error when all accounts are exhausted.
///
/// The retry hint implements summarized-failure retry (context
/// decontamination, arXiv:2605.08563): after a *model-behavior* failure
/// (timeout / empty response) the next attempt gets a one-line deterministic
/// summary to steer it away from the failed approach, instead of silently
/// re-running the byte-identical prompt. Infra failures (rate limit,
/// billing, auth, spawn) pass `None` so the prompt stays unchanged and
/// prompt-cache friendly.
///
/// `input_size_hint` is used for rough API-key cost accounting when the
/// spawn closure doesn't extract token usage from the CLI stream.
///
/// `account_pool` (the answering agent's `agent.toml [model] account_pool`)
/// narrows the rotator's *candidate set* only (see
/// [`AccountRotator::select_for_provider_with_pool`]); the rotation strategy,
/// the failure classification, and the cost accounting below are untouched.
/// `&[]` is the pre-pool behavior, byte-for-byte.
///
/// Note the deliberate interaction with the attempt budget: `max_attempts`
/// still counts *all* configured accounts, not just the pooled ones. When a
/// pooled account is exhausted mid-loop the rotator's fail-open rule hands
/// back the full set, and the remaining attempts can still land a reply —
/// availability beats the operator's preference, which is the whole point of
/// the fail-open semantics.
///
/// [`AccountRotator::select_for_provider_with_pool`]: duduclaw_agent::account_rotator::AccountRotator::select_for_provider_with_pool
pub(crate) async fn rotate_cli_spawn<F, Fut>(
    rotator: &duduclaw_agent::account_rotator::AccountRotator,
    account_pool: &[String],
    spawn: F,
    input_size_hint: usize,
) -> Result<String, String>
where
    F: Fn(std::collections::HashMap<String, String>, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let account_count = rotator.count().await;
    let max_attempts = account_count.max(1);
    let mut last_error = String::new();
    let mut retry_hint: Option<String> = None;

    for attempt in 0..max_attempts {
        let Some(selected) = rotator.select_with_pool(account_pool).await else {
            // WP10: distinguish "accounts ARE configured but none is currently
            // available" (all cooling down / marked unhealthy) from "the last
            // attempt failed with <error>". Previously both collapsed into
            // `All accounts exhausted. Last error: ` with an EMPTY tail, which
            // classified as Unknown and told the user to check debug.log.
            //
            // The genuinely-empty rotator (`account_count == 0`) keeps the
            // legacy aggregator string — it has its own callers and message.
            //
            // WP10 M4: tier the marker by the actual cooldown horizon so the
            // zh-TW message can say "a few minutes" vs "up to 24 hours"
            // instead of one hedged sentence. Unknown ⇒ conservative wording
            // covering both (the caller must not guess).
            if account_count > 0 && last_error.is_empty() {
                use duduclaw_agent::account_rotator::UnavailableReason;
                let tier = match rotator.unavailable_reason().await {
                    UnavailableReason::LongCooldown => "billing cooldown",
                    UnavailableReason::ShortCooldown => "short cooldown",
                    UnavailableReason::Unknown => "reason unknown",
                };
                return Err(format!(
                    "no accounts available: {tier} — all {account_count} configured \
                     account(s) are cooling down or marked unhealthy"
                ));
            }
            break;
        };
        info!(account = %selected.id, attempt, "Channel CLI attempt");

        match spawn(selected.env_vars.clone(), retry_hint.clone()).await {
            Ok(text) => {
                // Channel calls don't extract token usage from streams, so cost
                // is 0 (OAuth subscription) or a rough estimate (API key).
                let cost =
                    if selected.auth_method == duduclaw_agent::account_rotator::AuthMethod::OAuth {
                        0
                    } else {
                        ((input_size_hint + text.len()) / 1000).max(1) as u64
                    };
                rotator.on_success(&selected.id, cost).await;
                return Ok(text);
            }
            Err(e) => {
                last_error = e.clone();
                if crate::claude_runner::is_billing_error(&e) {
                    warn!(account = %selected.id, error = %e, "Account billing exhausted — 24h cooldown");
                    rotator.on_billing_exhausted(&selected.id).await;
                } else if crate::claude_runner::is_rate_limit_error(&e) {
                    warn!(account = %selected.id, error = %e, "Account rate-limited — cooldown");
                    rotator.on_rate_limited(&selected.id).await;
                } else if crate::pty_runtime::is_pty_transport_error(&e) {
                    // WP10 (2026-08-04 field incident): a wedged interactive
                    // REPL is a *transport* failure, not an account failure —
                    // the same OAuth account answers fine over fresh-spawn
                    // `claude -p`. Booking it against account health is what
                    // turned one 120 s stall into "All accounts exhausted" on
                    // single-account installs, killing every later message.
                    // Do NOT call `on_error` here; the PTY-pool wrapper
                    // handles it via the demotion breaker + fresh-spawn
                    // fallback.
                    warn!(
                        account = %selected.id,
                        error = %e,
                        "PTY transport failure — NOT counted against account health"
                    );
                } else if let Some(kind) = auth_failure_kind_for(&e) {
                    // D2 (2026-09-08 incident): an authentication failure is
                    // terminal until a human acts. `on_error`'s three-strike /
                    // 2-minute cycle handed the dead account straight back to
                    // the next message; `on_auth_failed` takes it out on the
                    // FIRST failure with a 15 min → 6 h backoff ladder.
                    warn!(
                        account = %selected.id,
                        error = %e,
                        kind = %kind,
                        "Account authentication failed — marking credential auth-dead"
                    );
                    rotator.on_auth_failed(&selected.id, kind).await;
                } else {
                    warn!(account = %selected.id, error = %e, "Account CLI attempt failed");
                    rotator.on_error(&selected.id).await;
                }
                retry_hint = retry_hint_for(&e);
            }
        }
    }

    Err(format!("All accounts exhausted. Last error: {last_error}"))
}

