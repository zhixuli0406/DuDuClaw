use super::*;

/// Core primitive: spawn the `claude` CLI subprocess with a streaming JSON reader.
///
/// `env_vars` allows the caller to inject per-account credentials
/// (e.g. `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CONFIG_DIR`, `ANTHROPIC_API_KEY`).
/// When `env_vars` is empty, falls back to the ambient env plus any
/// `ANTHROPIC_API_KEY` discovered via [`get_api_key`].
///
/// An empty-string value in `env_vars` is treated as a `remove` directive —
/// this matches `AccountRotator::select()` semantics (it emits an empty
/// `ANTHROPIC_API_KEY` to force OAuth paths not to leak an API key).
#[allow(clippy::too_many_arguments)] // pure extraction of existing call_claude_cli body
pub(super) async fn spawn_claude_cli_with_env(
    user_message: &str,
    model: &str,
    system_prompt: &str,
    home_dir: &Path,
    work_dir: Option<&Path>,
    on_progress: Option<&ProgressCallback>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    env_vars: &std::collections::HashMap<String, String>,
    claude_session_id: Option<&str>,
    // P1/WP-3: per-call reasoning effort. `None` ⇒ no `--effort` flag.
    effort: Option<duduclaw_core::effort::Effort>,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    // Find claude binary
    let claude_path =
        duduclaw_core::which_claude().ok_or_else(|| "claude CLI not found in PATH".to_string())?;

    // API key is optional — OAuth users authenticate via OS keychain.
    // Only set ANTHROPIC_API_KEY env var if we have one (as backup/override).
    // Skipped when the caller provides explicit env_vars (rotator path).
    let api_key = if env_vars.is_empty() {
        get_api_key(home_dir).await
    } else {
        None
    };

    let mut cmd = duduclaw_core::platform::async_command_for(&claude_path);

    // WP-8B (credentials doctrine P3, 2026-08): stop the child from
    // inheriting the gateway's full environment — that used to leak every
    // vendor `*_API_KEY` configured for ANY agent/provider on this gateway
    // into every `claude` CLI subprocess. Clear the env and seed only the
    // allowlisted base (see `duduclaw_core::spawn_env`); the `api_key` /
    // `env_vars` (rotator-resolved) applications further below run AFTER
    // this and always win.
    //
    // WP-10A (2026-08): `_for` additionally seeds
    // `SSH_AUTH_SOCK`/`SSH_AGENT_PID`/`GPG_TTY`/`GNUPGHOME` when this
    // agent's `agent.toml [capabilities] git_credentials = true` — default
    // `false` ⇒ byte-identical to the call above. Every spawn that actually
    // carries one of those names is audit-logged (names only, never
    // values).
    let git_env_granted = duduclaw_core::apply_agent_cli_env_allowlist_for(&mut cmd, capabilities);
    if !git_env_granted.is_empty() {
        let agent_id = work_dir
            .and_then(|d| d.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        duduclaw_security::audit::log_git_credentials_granted(home_dir, agent_id, &git_env_granted);
        // C1 producer 甲 companion — see `security_autopilot.rs`.
        crate::security_autopilot::emit_git_credentials_granted(agent_id);
    }

    // Resume an existing Claude CLI session for multi-turn continuity.
    // Placed before `-p` so the CLI establishes session context first.
    // Session ID is deterministic: SHA-256(duduclaw_session_id + account_id).
    if let Some(sid) = claude_session_id {
        cmd.args(["--resume", sid]);
    }

    cmd.args([
        // NOTE: `--bare` was previously used here to skip hooks/LSP/plugin-sync
        // for ~15-25% latency reduction. Removed because Claude CLI 2.1.110
        // regresses OAuth authentication when `--bare` is active — the flag
        // cuts the OS-keychain credential lookup alongside the optimizations,
        // causing every subprocess call to fail with "Not logged in".
        // The system prompt goes via `--system-prompt-file` (below), which
        // *replaces* the default system prompt and keeps it stable across turns
        // for prompt-cache reuse.
        //
        // WP-7A (bug1): `--exclude-dynamic-system-prompt-sections` was here but
        // is a documented no-op when combined with `--system-prompt[-file]`
        // ("Only applies with the default system prompt (ignored with
        // --system-prompt)" — CLI help). Local `claude -p` measurement
        // confirmed byte-identical token counts with and without it while
        // `--system-prompt-file` is set (2026-08-16). Removed.
        "-p",
        user_message,
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--verbose",
        // Channel subprocess has no TTY — bypass all permission prompts.
        "--dangerously-skip-permissions",
        // Allow enough agentic turns for complex tasks.
        "--max-turns",
        "50",
    ]);

    // P1/WP-3: per-call reasoning effort. `--effort <low|medium|high|xhigh|max>`
    // verified on Claude Code 2.1.258 (`--help`); it is per-invocation only and
    // is NOT persisted as the default. `None` ⇒ the flag is absent and this argv
    // is byte-identical to before effort existed.
    //
    // CACHE: effort participates in the cached prefix, so changing it
    // mid-conversation invalidates the prompt cache — hold it steady across a
    // conversation unless you mean to pay the rebuild.
    if let Some(effort) = effort {
        cmd.args([
            "--effort",
            effort
                .clamp_for(duduclaw_core::types::RuntimeType::Claude)
                .as_str(),
        ]);
    }

    // Apply tool restrictions based on agent capabilities (deny-by-default)
    {
        let caps = capabilities.cloned().unwrap_or_default();
        // HS12: enforce a per-agent allowlist when configured.
        // Inside the explore lane (`crate::explore_lane`, either source; this
        // path is the `ClaudeRuntime` choke-point a failover can reach) only
        // DuDuClaw MCP tools and read-only built-ins are auto-approved or present.
        let explore_lane = crate::explore_lane::claude_tools(&caps);
        let allowed = caps.allowed_tools();
        if let Some((_, lane_allowed)) = &explore_lane {
            cmd.args(["--allowedTools", &lane_allowed.join(",")]);
        } else if !allowed.is_empty() {
            cmd.args(["--allowedTools", &allowed.join(",")]);
        }
        let denied = caps.disallowed_tools();
        if !denied.is_empty() {
            let denied_csv = denied.join(",");
            cmd.args(["--disallowedTools", &denied_csv]);
        }
        // NOTE: `caps.browser_via_bash` no longer injects an env flag here.
        // The `bash-gate.sh` Layer 1.5 allowlist that read
        // `DUDUCLAW_BROWSER_VIA_BASH` was removed in `ba015a48`; the
        // capability still takes effect through `disallowed_tools()` above
        // and `CapabilitiesConfig::sandbox_level()` for the codex/gemini
        // runtimes. Setting a flag nothing reads is worse than not setting it.

        // RFC-23 §14.4: arm the data-file guard PreToolUse hook. Set ONLY
        // when redaction is actually active for this home AND the operator
        // has not turned the guard off — absent ⇒ the installed hook exits 0
        // immediately, so a gateway without redaction spawns exactly as it
        // did before §14.4.
        if let Some(mode) = crate::redaction_proxy::data_file_guard_env_for_spawn(home_dir) {
            cmd.env(duduclaw_core::ENV_DATA_FILE_GUARD, mode);
        }

        // WP-7A minimal-context: drop the operator's *user*-global settings and
        // memory (~14.8k tokens) and expose only a curated built-in tool subset
        // (~10k tokens) instead of the full ~21k built-in schema. `project,local`
        // is deliberate — dropping `user` alone removes the operator's personal
        // ~/.claude/CLAUDE.md/rules while KEEPING the agent's own
        // `.claude/settings.json` (the agent-file-guard PreToolUse hook still
        // loads; `--setting-sources ""` would silently disable it — verified by
        // a deny-hook probe, 2026-08-16). MCP tools are unaffected (they come
        // from --mcp-config, orthogonal to setting sources). Default ON; env
        // kill-switch DUDUCLAW_MINIMAL_CONTEXT / per-agent [runtime]
        // minimal_context = false opts out.
        if duduclaw_core::agent_toml::resolve_minimal_context(work_dir) {
            cmd.args(["--setting-sources", "project,local"]);
            if explore_lane.is_none() {
                let tools =
                    caps.minimal_builtin_tools(&duduclaw_core::types::CURATED_BUILTIN_TOOLS);
                cmd.args(["--tools", &tools.join(",")]);
            }
        }
        if let Some((lane_tools, _)) = &explore_lane {
            cmd.args(["--tools", &lane_tools.join(",")]);
            cmd.env(duduclaw_core::ENV_LANE, duduclaw_core::LANE_EXPLORE);
        }
    }
    // RFC-23 §13.6: when redaction is active this holds the per-spawn
    // rewritten `.mcp.json` (external stdio servers routed through
    // `duduclaw mcp-proxy`). Declared out here so the temp file outlives the
    // child — same discipline as `_prompt_guard` below.
    let _mcp_proxy_guard: Option<tempfile::TempPath>;
    // Set working directory to agent dir so Claude can access agent config
    // (.claude/, CLAUDE.md, .mcp.json) and project files (docs/, etc.)
    if let Some(dir) = work_dir {
        // `.mcp.json` platform fix: regenerate the DuDuClaw entry before the
        // CLI starts every server the file lists; a file that cannot be
        // confirmed refuses this spawn (fail closed, audited).
        {
            let dir_owned = dir.to_path_buf();
            match tokio::task::spawn_blocking(move || {
                duduclaw_agent::mcp_template::prepare_mcp_config_for_spawn(&dir_owned)
            })
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(msg)) => return Err(msg),
                Err(e) => {
                    return Err(duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
                        "MCP 設定檢查無法執行：{e}"
                    )));
                }
            }
        }
        // Install the agent-file-guard PreToolUse hook into
        // <agent_dir>/.claude/settings.json before spawning. This blocks
        // the sub-agent from using raw Write/Edit to create agent-structure
        // files (agent.toml/SOUL.md/…) outside <home>/agents/<name>/.
        // Best-effort — logs warning on failure but does not abort spawn.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) = crate::agent_hook_installer::ensure_agent_hook_settings(dir, &bin).await {
            warn!(
                agent_dir = %dir.display(),
                error = %e,
                "Failed to install agent-file-guard hook — spawn continuing without enforcement"
            );
        }
        cmd.current_dir(dir);

        // --bare disables .mcp.json auto-discovery, so explicitly specify it.
        // --strict-mcp-config ensures no ambient global MCP leaks into agent context.
        let mcp_json = dir.join(".mcp.json");
        if mcp_json.exists() {
            // RFC-23 §13.6: external MCP servers are launched by the Claude
            // CLI, so their tool results never pass DuDuClaw's MCP choke
            // point. With redaction active, hand the CLI a rewritten config
            // that routes each of them through `duduclaw mcp-proxy` instead.
            // `None` (redaction off / nothing to proxy) ⇒ byte-identical.
            // In the explore lane third-party servers are proxied too, so
            // they list and run only read tools (`crate::explore_lane`).
            _mcp_proxy_guard = crate::redaction_proxy::maybe_proxy_mcp_config_in_lane(
                home_dir,
                &mcp_json,
                crate::explore_lane::in_explore(),
            );
            match _mcp_proxy_guard.as_ref() {
                Some(proxied) => cmd.args(["--mcp-config", &proxied.to_string_lossy()]),
                None => cmd.args(["--mcp-config", &mcp_json.to_string_lossy()]),
            };
            cmd.arg("--strict-mcp-config");
        } else {
            _mcp_proxy_guard = None;
        }
    } else {
        _mcp_proxy_guard = None;
    }
    if let Some(ref key) = api_key {
        cmd.env("ANTHROPIC_API_KEY", key);
    }

    // Apply rotator-provided env vars (overrides any ambient/api_key values).
    // Empty-string values mean "remove this env var" — used by AccountRotator
    // to force OAuth paths to not leak a stale ANTHROPIC_API_KEY.
    for (key, value) in env_vars {
        if value.is_empty() {
            cmd.env_remove(key);
        } else {
            cmd.env(key, value);
        }
    }

    // Pass system prompt via temp file to avoid exposure in /proc/PID/cmdline (BE-C1)
    // CACHE_SPLIT_MARKER is a Direct-API-only layering hint — strip it here.
    let system_prompt_cli: std::borrow::Cow<'_, str> = if system_prompt
        .contains(crate::direct_api::CACHE_SPLIT_MARKER)
    {
        std::borrow::Cow::Owned(system_prompt.replace(crate::direct_api::CACHE_SPLIT_MARKER, ""))
    } else {
        std::borrow::Cow::Borrowed(system_prompt)
    };
    let system_prompt = system_prompt_cli.as_ref();
    let _prompt_guard: Option<tempfile::TempPath> = if !system_prompt.is_empty() {
        match tempfile::NamedTempFile::new() {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(system_prompt.as_bytes());
                let path = f.into_temp_path();
                cmd.args(["--system-prompt-file", &path.to_string_lossy()]);
                Some(path)
            }
            Err(_) => {
                cmd.args(["--system-prompt", system_prompt]);
                None
            }
        }
    } else {
        None
    };

    // Inject channel reply context for delegation callback forwarding.
    // The MCP `send_to_agent` tool reads this env var to register a callback
    // so sub-agent responses are forwarded back to the originating channel.
    if let Ok(channel) = crate::claude_runner::REPLY_CHANNEL.try_with(|ch| ch.clone()) {
        cmd.env(duduclaw_core::ENV_REPLY_CHANNEL, &channel);
    }

    // v1.10: Inject wiki RL trust feedback context so the MCP server can
    // forward turn_id / session_id into BusMessage when enqueueing
    // sub-agent dispatch. Without this, sub-agent RAG citations are not
    // attributed back to the originating turn's prediction error.
    if let Ok(Some(turn_id)) = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(|t| t.clone()) {
        cmd.env(duduclaw_core::ENV_TRUST_TURN_ID, &turn_id);
    }
    if let Ok(Some(session_id)) =
        duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(|s| s.clone())
    {
        cmd.env(duduclaw_core::ENV_TRUST_SESSION_ID, &session_id);
    }
    crate::memory_provenance::inject_turn_user_message_env(&mut cmd);
    // Record `(agent, turn) → reply channel` for as long as this CLI runs, so
    // the computer-use route can ask a human in THIS chat (and only here)
    // about a high-risk action of this employee. Dropped when this function
    // returns, i.e. when the turn's CLI is done.
    let _computer_use_turn = crate::claude_runner::CHANNEL_REPLY_AGENT_ID
        .try_with(|id| id.clone())
        .ok()
        .filter(|id| !id.is_empty())
        .and_then(|agent| crate::computer_use_sessions::turns::register_current_turn(&agent));

    // Prevent "nested session" error when gateway was launched from a Claude Code session
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

    // Drain stderr concurrently and keep the last ~2 KiB for error diagnostics.
    // Without draining, claude CLI may block if stderr pipe fills up (>64 KiB).
    let stderr_pipe = child.stderr.take();
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(pipe) = stderr_pipe {
        let buf = stderr_buf.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut reader = tokio::io::BufReader::new(pipe);
            let mut chunk = [0u8; 4096];
            while let Ok(n) = reader.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if let Ok(mut guard) = buf.lock() {
                    guard.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    // Keep only the last 2 KiB — we only need tail for diagnostics.
                    if guard.len() > 2048 {
                        let cut = guard.len() - 2048;
                        *guard = guard[cut..].to_string();
                    }
                }
            }
        });
    }

    // Optional raw-stream logging for deep debugging. Enable with
    // `DUDUCLAW_STREAM_DEBUG=1` in the gateway process environment — every
    // line from `claude`'s stdout is appended to `<home>/claude_stream.log`.
    // Intentionally off by default (can be large and contains prompts).
    let stream_debug = std::env::var("DUDUCLAW_STREAM_DEBUG")
        .map(|v| v == "1")
        .unwrap_or(false);
    let stream_debug_path = if stream_debug {
        Some(home_dir.join("claude_stream.log"))
    } else {
        None
    };

    // Split accumulators: `result` is authoritative when present, but a
    // stream that ends without one still has the assistant text.
    // `assistant_text` appends (a reply is a sequence of text blocks across
    // one or more `assistant` events); the terminal `result` event replaces.
    let mut assistant_text = String::new();
    let mut result_text = String::new();
    // RFC-22 P1-7: capture token usage from `result` event so cost_telemetry
    // can be recorded for channel-path replies (previously: 0 entries for
    // agnes despite 23-min runs because rotate_cli_spawn discarded usage).
    let mut token_usage: Option<crate::cost_telemetry::TokenUsage> = None;
    let mut utility_usage_recorded = false;
    // Track last tool type to suppress duplicate progress messages
    let mut last_tool_reported: Option<String> = None;
    // The model the CLI actually answered with (from `message.model`), reported
    // once via ProgressEvent::ModelInfo so the dashboard shows the real model.
    let mut reported_model: Option<String> = None;

    // Diagnostic counters — included in the "Empty response" error message
    // so the next occurrence is immediately actionable (no more needing to
    // reproduce manually in a shell).
    let mut lines_seen: u32 = 0;
    let mut events_parsed: u32 = 0;
    let mut assistant_events: u32 = 0;
    let mut text_blocks: u32 = 0;
    let mut thinking_blocks: u32 = 0;
    let mut tool_use_blocks: u32 = 0;
    let mut result_events: u32 = 0;
    let mut last_raw_line: String = String::new();
    let mut last_result_subtype: Option<String> = None;
    let mut last_stop_reason: Option<String> = None;

    // C-P1: converts tool_use / tool_result stream-json events into ordered
    // start/end step events for the dashboard's agentic task tree. Runs
    // alongside the existing ToolUse/TodoUpdate progress emission below.
    let mut step_tracker = StepTracker::new();

    // Task C (O-4 Guide-path result cards): only a `system_operator`-capable
    // agent's turn pays for this — everyone else's stream loop takes the
    // exact same branches it always did (byte-identical output). Reuses the
    // WP-A4 pairing logic (`claude_runner::ingest_stream_json_event_for_native_tools`)
    // instead of duplicating tool_use/tool_result matching a second time in
    // this file; the resulting events are flushed into whatever
    // `NATIVE_TOOL_COLLECTOR` scope the caller entered (see
    // `build_guarded_reply_for_agent`/`build_guarded_reply_with_session`),
    // same best-effort, scope-optional contract every other producer of that
    // collector already relies on.
    let operator_result_capture = capabilities.map(|c| c.system_operator).unwrap_or(false);
    let mut operator_native_events: Vec<crate::runtime::NativeToolEvent> = Vec::new();
    let mut operator_open_calls: Vec<(String, usize)> = Vec::new();

    // R1: deterministic, zero-LLM-cost trajectory anomaly detector. Fed the
    // same start/end step stream as the dashboard tree. Default: report-only
    // (append high-severity signals to channel_failures.jsonl); it NEVER kills
    // the task. Config lives in <home>/config.toml [trajectory_guard].
    let mut traj_guard = crate::trajectory_guard::TrajectoryGuard::from_home(home_dir);
    let traj_agent = work_dir
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let traj_session = claude_session_id.unwrap_or("").to_string();

    // G12 step persistence: tee StepTracker starts + TodoWrite boards into
    // the bounded `run_steps.db` so the run inspector can replay real tool
    // steps (previously live-streamed only, gone after the reply). Strictly
    // additive and best-effort: the existing streaming / edit-in-place
    // behavior is untouched, and any open/insert failure is a debug log +
    // drop — never a blocked or failed reply. Only turns that carry the
    // channel session key (task-local, set by build_reply_with_session_inner)
    // persist; other spawn paths have no run to attach steps to and skip.
    let step_session_key: Option<String> = duduclaw_memory::feedback::CURRENT_SESSION_ID
        .try_with(|s| s.clone())
        .ok()
        .flatten();
    let step_store = step_session_key
        .as_ref()
        .and_then(|_| crate::run_steps::shared_store(home_dir));
    // Agent attribution mirrors cost_telemetry (task-local first), with the
    // work-dir name as the fallback the trajectory guard already uses.
    let step_agent_id = crate::claude_runner::CHANNEL_REPLY_AGENT_ID
        .try_with(|a| a.clone())
        .ok()
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| traj_agent.clone());
    // Per-invocation monotonic sequence — orders same-second events.
    let mut step_seq: i64 = 0;

    // R2: deterministic early-failure-warning scorer over the trajectory
    // *prefix* (foresight). Report-only like R1: warning → Activity Feed +
    // channel_failures record; critical → additionally a `run.at_risk`
    // event for the autopilot bus. Never blocks or kills the run; every
    // internal failure is fail-safe (no alarm). Config: [foresight].
    let mut foresight = crate::foresight::ForesightScorer::from_home(home_dir, &traj_agent);

    // Keepalive timer — fires periodically when no stream events arrive
    let mut keepalive =
        tokio::time::interval(std::time::Duration::from_secs(KEEPALIVE_INTERVAL_SECS));
    keepalive.reset(); // don't fire immediately

    // Hard max timeout — absolute safety net
    let hard_deadline = tokio::time::sleep(std::time::Duration::from_secs(HARD_MAX_TIMEOUT_SECS));
    tokio::pin!(hard_deadline);

    loop {
        tokio::select! {
            // Priority 1: read stream-json events from CLI stdout
            line_result = reader.next_line() => {
                match line_result {
                    // Stream ended normally
                    Ok(None) => break,
                    // Read error
                    Err(e) => {
                        let _ = child.kill().await;
                        return Err(format!("claude CLI read error: {e}"));
                    }
                    // Got a line — parse stream-json event
                    Ok(Some(line)) => {
                        // Reset keepalive timer on every received line
                        keepalive.reset();

                        if line.trim().is_empty() {
                            continue;
                        }

                        lines_seen += 1;
                        // Keep only a truncated tail for diagnostics (full line
                        // can contain the user's prompt — we don't want it on disk).
                        // `rate_limit_event` frames are excluded: embedding one in a
                        // failure diagnostic made `is_rate_limit_error` classify a
                        // healthy account as rate-limited ("rateLimitType" ⊃
                        // "ratelimit" — TODO-rate-limit-warning-misread-as-failure).
                        if !crate::rate_limit_watch::line_is_rate_limit_frame(&line) {
                            last_raw_line = line.chars().take(400).collect();
                        }

                        // Optional raw-stream debug log.
                        if let Some(ref p) = stream_debug_path {
                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(p)
                            {
                                use std::io::Write;
                                let _ = writeln!(f, "{line}");
                            }
                        }

                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                            events_parsed += 1;

                            // R2: feed the foresight scorer the raw event (it
                            // extracts tool_use / tool_result / TodoWrite itself)
                            // and emit at most one new alarm per threshold.
                            foresight.observe_event(&event);
                            if let Some(alarm) = foresight.check() {
                                crate::foresight::emit_alarm(
                                    home_dir, &traj_agent, &traj_session, &alarm,
                                );
                            }

                            // Task C: accumulate this event's tool_use/tool_result
                            // into the operator-result collector — gated on
                            // `operator_result_capture` so a non-operator agent's
                            // loop never allocates or masks anything extra here.
                            if operator_result_capture {
                                crate::claude_runner::ingest_stream_json_event_for_native_tools(
                                    &event,
                                    &mut operator_native_events,
                                    &mut operator_open_calls,
                                );
                            }

                            // C-P1: emit structured start/end step events for the
                            // dashboard task tree. Additive — leaves the text token
                            // stream and the ToolUse/TodoUpdate emission untouched.
                            // R1: the same step stream feeds the trajectory guard
                            // (runs even when no on_progress callback is attached).
                            for step in step_tracker.ingest(&event) {
                                // G12: persist the Start boundary (tool name +
                                // the same CJK-capped args summary the live
                                // step tree shows). End boundaries add no
                                // transcript value and are not persisted.
                                // TodoWrite is persisted separately as a richer
                                // `todo_update` event (board snapshot) — skip it
                                // here so the transcript doesn't show two cards
                                // (a bare tool_step + the board) for one call.
                                if step.phase == StepPhase::Start && step.tool != "TodoWrite" {
                                    if let (Some(store), Some(key)) =
                                        (step_store.as_deref(), step_session_key.as_deref())
                                    {
                                        step_seq += 1;
                                        store.append_best_effort(
                                            &step_agent_id,
                                            key,
                                            crate::run_steps::KIND_TOOL_STEP,
                                            &step.tool,
                                            step.summary.as_deref().unwrap_or(""),
                                            step_seq,
                                        );
                                    }
                                }
                                let obs = crate::trajectory_guard::ToolStep::from(&step);
                                for sig in traj_guard.observe_step(&obs) {
                                    if sig.severity == crate::trajectory_guard::Severity::High {
                                        let intervene = traj_guard.should_intervene(&sig);
                                        warn!(
                                            agent = %traj_agent,
                                            anomaly = sig.kind.as_str(),
                                            evidence = %sig.evidence,
                                            intervene,
                                            "trajectory guard: 偵測到高風險軌跡異常（僅上報，不中止任務）"
                                        );
                                        let rec = crate::trajectory_guard::anomaly_record(
                                            &traj_agent, &traj_session, &sig, intervene,
                                        );
                                        if let Err(e) =
                                            crate::trajectory_guard::append_anomaly(home_dir, &rec)
                                        {
                                            warn!(error = %e, "trajectory guard: 寫入 channel_failures.jsonl 失敗");
                                        }
                                    }
                                }
                                if let Some(cb) = on_progress {
                                    cb(ProgressEvent::Step(step));
                                }
                            }

                            // L5 §14: count real invocations of approved custom
                            // skills. Only fires when the event carries a `Skill`
                            // tool_use; the slug set is 60s-cached and the increment
                            // is detached so it never delays the reply. Token-equal
                            // slug match only (no substring).
                            let skill_names = extract_skill_tool_names(&event);
                            if !skill_names.is_empty() {
                                let approved = approved_custom_skill_slugs(home_dir).await;
                                for name in skill_names {
                                    if let Some(slug) = matched_custom_slug(&name, &approved) {
                                        let home = home_dir.to_path_buf();
                                        let slug = slug.to_string();
                                        tokio::spawn(async move {
                                            match crate::custom_skills::CustomSkillStore::open(&home) {
                                                Ok(store) => {
                                                    if let Err(e) =
                                                        store.increment_usage_by_slug(&slug).await
                                                    {
                                                        warn!(slug = %slug, error = %e, "custom skill usage increment failed");
                                                    }
                                                }
                                                Err(e) => warn!(error = %e, "open custom skill store for usage increment failed"),
                                            }
                                        });
                                    }
                                }
                            }

                            match event.get("type").and_then(|t| t.as_str()) {
                                // Final result event — contains the complete response.
                                //
                                // CRITICAL: the stream-json schema signals terminal
                                // errors via `is_error: true` on the `result` event
                                // (e.g. "Not logged in · Please run /login", auth
                                // failures, rate limits surfaced as synthetic replies).
                                // Without this check we would swallow the error text
                                // into `result_text` and return Ok to the caller.
                                Some("result") => {
                                    result_events += 1;
                                    // Utility CLI calls carry their own caller
                                    // scope, rather than masquerading as channel
                                    // turns. Record real usage before error exits
                                    // and await the write before review can settle.
                                    if !utility_usage_recorded {
                                        utility_usage_recorded = crate::runtime_dispatch::record_claude_utility_result(
                                            home_dir, &event, reported_model.as_deref(),
                                        ).await;
                                    }
                                    last_result_subtype = event
                                        .get("subtype")
                                        .and_then(|s| s.as_str())
                                        .map(String::from);
                                    let is_error = event
                                        .get("is_error")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    if is_error {
                                        let err_text = event
                                            .get("result")
                                            .and_then(|r| r.as_str())
                                            .unwrap_or("Unknown stream-json error");
                                        let _ = child.kill().await;
                                        // Include captured stderr tail in the error so we can
                                        // diagnose cases where Claude CLI sets is_error=true
                                        // without a meaningful `result` text (e.g. --resume
                                        // failures, internal CLI errors). Without this the
                                        // error just says "Unknown stream-json error".
                                        let stderr_tail = stderr_buf
                                            .lock()
                                            .ok()
                                            .map(|g| g.trim().to_string())
                                            .filter(|s| !s.is_empty())
                                            .map(|s| {
                                                let snippet = duduclaw_core::truncate_bytes(&s, 500);
                                                format!(" | stderr: {snippet}")
                                            })
                                            .unwrap_or_default();
                                        return Err(format!(
                                            "claude CLI stream error: {err_text}{stderr_tail}"
                                        ));
                                    }
                                    if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                                        // Only overwrite with the result event's text if it's
                                        // non-empty. When Claude uses tools, the final `result`
                                        // event often has `result: ""` because the real answer
                                        // was emitted in intermediate assistant text blocks.
                                        // Overwriting with "" would discard those responses and
                                        // trigger a false "Empty response" error.
                                        if !text.is_empty() {
                                            result_text = text.to_string();
                                        }
                                    }
                                    // RFC-22 P1-7: extract token usage from the result
                                    // event. Mirrors claude_runner.rs:1006 (dispatch path)
                                    // so channel and dispatch paths use identical
                                    // accounting. result event is the canonical source
                                    // — fall back to /message/usage on assistant events
                                    // is left for future work if needed.
                                    if let Some(usage_val) = event.get("usage") {
                                        token_usage =
                                            crate::cost_telemetry::TokenUsage::from_json(usage_val);
                                        // R1: feed a cumulative-cost sample to the
                                        // trajectory guard. A single-reply stream
                                        // usually emits one usage event, so the
                                        // slope rule mainly guards multi-result
                                        // streams; single samples never trip.
                                        if let Some(u) = token_usage.as_ref() {
                                            let sample = crate::trajectory_guard::CostSample {
                                                ts_ms: now_unix_ms(),
                                                cumulative: u.estimated_cost_millicents(),
                                            };
                                            // R2: same sample feeds the foresight
                                            // cost-slope feature.
                                            foresight
                                                .observe_cost(sample.ts_ms, sample.cumulative);
                                            if let Some(alarm) = foresight.check() {
                                                crate::foresight::emit_alarm(
                                                    home_dir,
                                                    &traj_agent,
                                                    &traj_session,
                                                    &alarm,
                                                );
                                            }
                                            for sig in traj_guard.observe_cost(sample) {
                                                if sig.severity
                                                    == crate::trajectory_guard::Severity::High
                                                {
                                                    let intervene =
                                                        traj_guard.should_intervene(&sig);
                                                    warn!(
                                                        agent = %traj_agent,
                                                        anomaly = sig.kind.as_str(),
                                                        evidence = %sig.evidence,
                                                        intervene,
                                                        "trajectory guard: 偵測到高風險成本斜率（僅上報，不中止任務）"
                                                    );
                                                    let rec =
                                                        crate::trajectory_guard::anomaly_record(
                                                            &traj_agent,
                                                            &traj_session,
                                                            &sig,
                                                            intervene,
                                                        );
                                                    if let Err(e) =
                                                        crate::trajectory_guard::append_anomaly(
                                                            home_dir, &rec,
                                                        )
                                                    {
                                                        warn!(error = %e, "trajectory guard: 寫入 channel_failures.jsonl 失敗");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                // Assistant message with content blocks
                                Some("assistant") => {
                                    assistant_events += 1;
                                    // Also check the envelope-level `error` field that
                                    // newer claude-code versions emit alongside the
                                    // synthetic assistant message on auth failure.
                                    if let Some(err) = event.get("error").and_then(|e| e.as_str()) {
                                        let _ = child.kill().await;
                                        return Err(format!(
                                            "claude CLI assistant error: {err}"
                                        ));
                                    }
                                    // Capture stop_reason for diagnostics (max_tokens,
                                    // tool_use, end_turn, stop_sequence, ...).
                                    if let Some(sr) = event
                                        .pointer("/message/stop_reason")
                                        .and_then(|v| v.as_str())
                                    {
                                        last_stop_reason = Some(sr.to_string());
                                    }
                                    // Surface the model the CLI ACTUALLY used —
                                    // may differ from the requested `--model`
                                    // (tier substitution, alias resolution).
                                    if let Some(m) = event
                                        .pointer("/message/model")
                                        .and_then(|v| v.as_str())
                                        .filter(|m| crate::runtime_dispatch::is_observed_claude_model(m))
                                    {
                                        if reported_model.as_deref() != Some(m) {
                                            reported_model = Some(m.to_string());
                                            if let Some(cb) = on_progress {
                                                cb(ProgressEvent::ModelInfo {
                                                    model: m.to_string(),
                                                });
                                            }
                                        }
                                    }
                                    if let Some(content) = event
                                        .pointer("/message/content")
                                        .and_then(|c| c.as_array())
                                    {
                                        for block in content {
                                            let block_type = block.get("type").and_then(|t| t.as_str());
                                            match block_type {
                                                Some("text") => {
                                                    text_blocks += 1;
                                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                        // Append, never replace: overwriting kept
                                                        // only the LAST fragment of a long reply,
                                                        // which reached the user as a few
                                                        // characters with a two-digit token count.
                                                        assistant_text.push_str(text);
                                                    }
                                                }
                                                Some("thinking") => {
                                                    thinking_blocks += 1;
                                                }
                                                Some("tool_use") => {
                                                    tool_use_blocks += 1;
                                                    // G12: persist TodoWrite boards as todo_update
                                                    // snapshots (independent of on_progress, so
                                                    // callback-less spawns are covered too). The
                                                    // preview is the SAME already-rendered board
                                                    // the channels display — no raw args.
                                                    if let (Some(store), Some(key)) =
                                                        (step_store.as_deref(), step_session_key.as_deref())
                                                    {
                                                        if block.get("name").and_then(|n| n.as_str())
                                                            == Some("TodoWrite")
                                                        {
                                                            if let Some(todos) = block
                                                                .get("input")
                                                                .and_then(parse_todo_write_input)
                                                            {
                                                                let done = todos
                                                                    .iter()
                                                                    .filter(|t| t.status == "completed")
                                                                    .count();
                                                                step_seq += 1;
                                                                store.append_best_effort(
                                                                    &step_agent_id,
                                                                    key,
                                                                    crate::run_steps::KIND_TODO_UPDATE,
                                                                    &format!("{done}/{}", todos.len()),
                                                                    &render_todo_list(&todos),
                                                                    step_seq,
                                                                );
                                                            }
                                                        }
                                                    }
                                                    // Extract tool name and detail for progress
                                                    if let Some(cb) = on_progress {
                                                        let tool = block.get("name")
                                                            .and_then(|n| n.as_str())
                                                            .unwrap_or("unknown")
                                                            .to_string();

                                                        // TodoWrite carries the agent's live task
                                                        // list — surface it as a progress board
                                                        // instead of a generic "using tool" line.
                                                        if tool == "TodoWrite" {
                                                            if let Some(todos) = block
                                                                .get("input")
                                                                .and_then(parse_todo_write_input)
                                                            {
                                                                cb(ProgressEvent::TodoUpdate { todos });
                                                                last_tool_reported = Some(tool);
                                                                continue;
                                                            }
                                                        }

                                                        let detail = extract_tool_detail(block);

                                                        // Suppress duplicate: same tool consecutively
                                                        let dominated = last_tool_reported
                                                            .as_ref()
                                                            .is_some_and(|prev| *prev == tool && detail.is_none());
                                                        if !dominated {
                                                            cb(ProgressEvent::ToolUse {
                                                                tool: tool.clone(),
                                                                detail,
                                                            });
                                                            last_tool_reported = Some(tool);
                                                        }
                                                    }
                                                }
                                                _ => {} // tool_result, etc.
                                            }
                                        }
                                    }
                                }
                                Some("rate_limit_event") => {
                                    // Quota advisory — telemetry, never a failure.
                                    // The run continues; only record and move on.
                                    crate::rate_limit_watch::record_frame(&event);
                                }
                                _ => {} // system, etc.
                            }
                        }
                    }
                }
            }

            // Priority 2: keepalive timer — send progress if silent too long
            _ = keepalive.tick() => {
                if let Some(cb) = on_progress {
                    cb(ProgressEvent::Keepalive);
                }
            }

            // Priority 3: hard max timeout — kill truly hung processes
            _ = &mut hard_deadline => {
                warn!(
                    "claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s) — killing process"
                );
                let _ = child.kill().await;
                if result_text.is_empty() && !assistant_text.is_empty() {
                    // No authoritative `result` text (tool-use turn, or the CLI
                    // omitted it) — the accumulated assistant prose IS the reply.
                    result_text = std::mem::take(&mut assistant_text);
                }
                if result_text.is_empty() {
                    return Err(format!(
                        "claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s, no output)"
                    ));
                }
                warn!(
                    "claude CLI hard timeout — returning partial result ({} chars)",
                    result_text.len()
                );
                break;
            }
        }
    }

    // Wait for process to exit
    let status = child.wait().await.map_err(|e| format!("wait error: {e}"))?;

    // Snapshot stderr tail for error diagnostics.
    let stderr_tail: String = stderr_buf
        .lock()
        .ok()
        .map(|g| g.chars().take(400).collect::<String>())
        .unwrap_or_default();

    // Compose the diagnostic summary that all error sites below embed.
    // With this in the error string, `channel_failures.jsonl` becomes
    // self-describing: we can tell whether the CLI produced any output
    // at all, whether it only produced thinking, whether stop_reason
    // was "max_tokens" / "tool_use", etc.
    let diag = format!(
        "exit={} lines={lines_seen} events={events_parsed} \
         assistant={assistant_events} text_blocks={text_blocks} \
         thinking={thinking_blocks} tool_use={tool_use_blocks} \
         result_events={result_events} \
         result_subtype={:?} stop_reason={:?} \
         last_line={:?} stderr_tail={:?}",
        status.code().unwrap_or(-1),
        last_result_subtype,
        last_stop_reason,
        last_raw_line,
        stderr_tail,
    );

    // Any non-zero exit is now a hard failure. Previously we only errored
    // when `result_text.is_empty()`, which hid synthetic error messages
    // (e.g. "Not logged in · Please run /login") that Claude CLI emits as
    // a real result event with `is_error: true` and exit code 1. The
    // stream-json error check above should have caught those before we
    // reach here, but the exit-code gate is a defensive backstop.
    if !status.success() {
        return Err(format!(
            "claude CLI exit {} ({diag})",
            status.code().unwrap_or(-1)
        ));
    }

    // Normal completion: with no authoritative `result` text (tool-use turns,
    // or a CLI that omits the event), the accumulated assistant prose IS the
    // reply. Folding it in here is what stops a long answer from arriving as
    // its last fragment.
    let mut result_text = result_text;
    if result_text.is_empty() && !assistant_text.is_empty() {
        result_text = std::mem::take(&mut assistant_text);
    }
    let result_text = result_text.trim().to_string();
    if result_text.is_empty() {
        return Err(format!("Empty response from claude CLI ({diag})"));
    }

    // OTel GenAI: post-hoc usage recording onto the active `invoke_agent`
    // span (fields declared Empty at the instrumented entry — see
    // `crate::otel`). No-op when the span is disabled or lacks the fields.
    if let Some(usage) = token_usage.as_ref() {
        let span = tracing::Span::current();
        span.record(crate::otel::attrs::USAGE_INPUT_TOKENS, usage.input_tokens);
        span.record(crate::otel::attrs::USAGE_OUTPUT_TOKENS, usage.output_tokens);
    }

    // RFC-22 P1-7: record cost_telemetry for the channel reply. Skipped when
    // the task_local agent_id is unset (e.g. invoked outside channel_reply,
    // such as the dispatch path which already records via claude_runner).
    if !crate::runtime_dispatch::is_claude_utility_call()
        && let (Some(usage), Ok(agent_id)) = (
        token_usage.as_ref(),
        crate::claude_runner::CHANNEL_REPLY_AGENT_ID.try_with(|id| id.clone()),
    ) {
        if !agent_id.is_empty()
            && let Some(telemetry) = crate::cost_telemetry::get_telemetry()
        {
            // WP6: attribute this spend to the end-user + channel when the
            // channel_reply path scoped them (empty ⇒ unattributed / system).
            let user_id = crate::claude_runner::CHANNEL_REPLY_USER_ID
                .try_with(|u| u.clone())
                .ok()
                .filter(|u| !u.is_empty());
            let channel = crate::claude_runner::REPLY_CHANNEL
                .try_with(|c| c.clone())
                .ok()
                .filter(|c| !c.is_empty());
            // WP5: thread the compression outcome computed in
            // `maybe_compress_history` (scoped as a task-local — this call
            // happens several async frames away from that computation) so
            // `token_usage.compressed` / `compression_stages` reflect what
            // was actually sent for this request. Missing scope (e.g. this
            // fn invoked outside the channel_reply path) falls back to
            // "not compressed", matching pre-WP5 behaviour.
            let compression = crate::prompt_compression::CHANNEL_REPLY_COMPRESSION
                .try_with(|c| c.clone())
                .unwrap_or_default();
            telemetry
                .record_attributed_with_compression(
                    &agent_id,
                    crate::cost_telemetry::RequestType::Chat,
                    model,
                    usage,
                    user_id.as_deref(),
                    channel.as_deref(),
                    compression.compressed,
                    &compression.stages,
                )
                .await;
        }
    }

    // Task C: best-effort flush into whatever `NATIVE_TOOL_COLLECTOR` scope
    // the caller entered (silent no-op with no scope, or for a non-operator
    // agent whose loop above never populated this vec — see
    // `extend_native_tool_events`'s own doc comment). Never affects the
    // primary CLI response either way.
    if operator_result_capture {
        crate::runtime::extend_native_tool_events(operator_native_events);
    }

    Ok(result_text)
}
