//! Shared helper for calling the Claude CLI (Claude Code SDK) on behalf of an agent.
//!
//! Used by both the cron scheduler and the agent dispatcher.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use duduclaw_agent::registry::AgentRegistry;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::llm_fallback::{
    emit_llm_fallback_audit, format_fallback_error_message, is_llm_fallback_error,
    should_attempt_model_fallback,
};

/// Shared `Arc<TaskStore>` injected by `server.rs` at startup so
/// `build_pending_tasks_section` reuses the gateway-owned connection
/// instead of opening a fresh SQLite connection per agent invocation.
///
/// The fallback path (open-per-call) is kept for tests and for graceful
/// degradation if the store injection somehow fails to run.
static SHARED_TASK_STORE: OnceLock<Arc<crate::task_store::TaskStore>> = OnceLock::new();

/// Register the shared `TaskStore` for use by `build_pending_tasks_section`.
/// Idempotent — only the first call takes effect. Called once from `server.rs`
/// after the store is opened.
pub fn set_shared_task_store(store: Arc<crate::task_store::TaskStore>) {
    let _ = SHARED_TASK_STORE.set(store);
}

/// Build a system prompt from an agent's loaded markdown files.
///
/// Skills are sorted alphabetically by name to ensure deterministic byte
/// sequences across calls — this maximizes prompt cache hit rates.
///
/// `citation_ctx`: when present, wiki pages injected here are recorded into
/// the global `CitationTracker` keyed by `(agent_id, turn_id, session_id)`.
/// `session_id` is None when the dispatcher chain doesn't carry session
/// context (e.g. cron-triggered tasks); the per-conversation cap then
/// degrades to a per-turn cap, which is conservative.
/// (review B2 — sub-agent dispatch was previously bypassing trust feedback.)
fn build_system_prompt(
    agent: &duduclaw_agent::LoadedAgent,
    citation_ctx: Option<(&str, &str, Option<&str>)>,
) -> String {
    // #11 (2026-05-12) — Minimal mode shortcut. Same opt-in flag as the
    // channel_reply path so an agent's mode choice is global. Cron path
    // routes through here, so flipping the flag also covers cron.
    if agent.config.prompt.mode == duduclaw_core::types::PromptMode::Minimal {
        let sender_block = ""; // citation_ctx doesn't carry a sender — minimal omits.
        let pinned = "";
        return crate::prompt_minimal::build_minimal_system_prompt(
            agent,
            sender_block,
            pinned,
        );
    }

    let mut parts = Vec::new();
    // Mirror `parts` with labelled byte counts for the prompt-size audit
    // log. Cheap (one usize per push) and gives operators per-section
    // visibility when the 200K cliff fires.
    let mut audit: Vec<crate::prompt_audit::PromptSection> = Vec::new();

    if let Some(soul) = &agent.soul {
        let s = format!("# Soul\n{}", soul.trim_end());
        audit.push(crate::prompt_audit::PromptSection::new("soul", &s));
        parts.push(s);
    }
    if let Some(identity) = &agent.identity {
        let s = format!("# Identity\n{}", identity.trim_end());
        audit.push(crate::prompt_audit::PromptSection::new("identity", &s));
        parts.push(s);
    }

    // Sort skills by name for deterministic ordering (cache-friendly).
    // #6.2b: cap the unbounded loop at DEFAULT_LEGACY_SKILL_BYTE_CAP so
    // an over-stuffed `SKILLS/` directory can't single-handedly push the
    // system prompt past the 200K cliff. Truncation footer surfaces the
    // omitted skills so it's debuggable rather than mysterious.
    let mut skills: Vec<_> = agent.skills.iter().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    let pairs: Vec<(String, String)> = skills
        .iter()
        .map(|s| (s.name.clone(), s.content.trim_end().to_string()))
        .collect();
    let (rendered, footer) = crate::prompt_audit::budgeted_legacy_skills(
        &pairs,
        crate::prompt_audit::DEFAULT_LEGACY_SKILL_BYTE_CAP,
    );
    let mut skills_total_bytes: usize = 0;
    for s in rendered {
        skills_total_bytes += s.len();
        parts.push(s);
    }
    if let Some(note) = footer {
        skills_total_bytes += note.len();
        parts.push(note);
    }
    if skills_total_bytes > 0 {
        audit.push(crate::prompt_audit::PromptSection {
            label: "skills",
            bytes: skills_total_bytes,
        });
    }

    if let Some(memory) = &agent.memory {
        let s = format!("# Memory\n{}", memory.trim_end());
        audit.push(crate::prompt_audit::PromptSection::new("memory", &s));
        parts.push(s);
    }

    // Wiki knowledge injection — L0 (Identity) + L1 (Core) pages
    let wiki_dir = agent.dir.join("wiki");
    if wiki_dir.exists() {
        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let result = match citation_ctx {
            Some((agent_id, turn_id, session_id)) => {
                let tracker = duduclaw_memory::feedback::global_tracker();
                store.build_injection_context_with_citations(
                    6000, agent_id, turn_id, session_id, &tracker,
                )
            }
            None => store.build_injection_context(6000),
        };
        match result {
            Ok(wiki_ctx) if !wiki_ctx.is_empty() => {
                let s = format!("# Wiki Knowledge\n{}", wiki_ctx.trim_end());
                audit.push(crate::prompt_audit::PromptSection::new("wiki", &s));
                parts.push(s);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Wiki injection failed in dispatcher: {e}");
            }
        }
    }

    // Behavioral contract boundaries — must_not / must_always rules.
    let contract_prompt = duduclaw_agent::contract::contract_to_prompt(&agent.contract);
    if !contract_prompt.is_empty() {
        audit.push(crate::prompt_audit::PromptSection::new(
            "contract",
            &contract_prompt,
        ));
        parts.push(contract_prompt);
    }

    crate::prompt_audit::maybe_log_breakdown(
        &agent.config.agent.name,
        "claude_runner",
        &audit,
        crate::prompt_audit::DEFAULT_EMIT_THRESHOLD_BYTES,
    );

    parts.join("\n\n---\n\n")
}

/// Build a concise "## Your Task Queue" section from the Task Board.
///
/// Pulls up to 5 open tasks (in_progress → todo → blocked, ordered by
/// priority urgent→low) assigned to `agent_id` and renders a bullet list
/// plus a reminder of the MCP tools available for task management.
///
/// Returns `None` when the agent has no pending tasks — callers should
/// skip appending the section in that case to keep the prompt tight.
async fn build_pending_tasks_section(home_dir: &Path, agent_id: &str) -> Option<String> {
    // Prefer the shared store (one SQLite connection for the whole
    // gateway process — avoids WAL write-lock contention on high-volume
    // channel replies). Fall back to per-call open only when the
    // injection hasn't run yet (tests, or a race at startup).
    let shared = SHARED_TASK_STORE.get().cloned();
    let fallback_store;
    let store: &crate::task_store::TaskStore = match shared.as_deref() {
        Some(s) => s,
        None => {
            fallback_store = match crate::task_store::TaskStore::open(home_dir) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        agent = %agent_id,
                        error = %e,
                        "task queue omitted from system prompt — TaskStore open failed"
                    );
                    return None;
                }
            };
            &fallback_store
        }
    };

    let mut all: Vec<crate::task_store::TaskRow> = Vec::new();
    for status in &["in_progress", "todo", "blocked"] {
        if let Ok(mut rows) = store.list_tasks(Some(status), Some(agent_id), None).await {
            all.append(&mut rows);
        }
    }
    if all.is_empty() {
        return None;
    }

    let priority_rank = |p: &str| match p {
        "urgent" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    };
    all.sort_by(|a, b| priority_rank(&a.priority).cmp(&priority_rank(&b.priority)));

    let total = all.len();
    let shown: Vec<String> = all
        .iter()
        .take(5)
        .enumerate()
        .map(|(i, t)| {
            let extra = match t.status.as_str() {
                "blocked" => t
                    .blocked_reason
                    .as_deref()
                    .map(|r| format!(" — blocked: {r}"))
                    .unwrap_or_default(),
                "in_progress" => " [in progress]".to_string(),
                _ => String::new(),
            };
            format!("{}. [{}] {}{}", i + 1, t.priority, t.title, extra)
        })
        .collect();
    let more = if total > 5 {
        format!("\n+{} more — call tasks_list to see all", total - 5)
    } else {
        String::new()
    };
    Some(format!(
        "## Your Task Queue ({total} pending)\n{}{}\n\n\
         Use `tasks_list`, `tasks_claim`, `tasks_update`, `tasks_complete`, `tasks_block` \
         to manage these, and `activity_post` to report progress without changing status.",
        shown.join("\n"),
        more,
    ))
}

/// Resolve the effective working directory for a Claude CLI subprocess.
///
/// If L0 worktree isolation is active (task-local `WORKTREE_PATH` is set),
/// use the worktree path. Otherwise fall back to the agent's base directory.
fn effective_work_dir(agent_dir: &Path) -> Option<PathBuf> {
    // Check worktree task-local first.
    let wt = WORKTREE_PATH.try_with(|opt| opt.clone()).ok().flatten();
    if let Some(ref p) = wt {
        if p.exists() {
            return Some(p.clone());
        }
    }
    agent_dir.exists().then(|| agent_dir.to_path_buf())
}

/// Look up an agent from the registry and route to the best model.
///
/// Routing logic per agent:
/// 1. If agent has `model.local` with `prefer_local = true` and local engine is available
///    → try local inference first
/// 2. If local fails or is not configured → fall back to Claude Code SDK via AccountRotator
///
/// Local inference and account rotation are completely separate paths.
pub async fn call_claude_for_agent(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
) -> Result<String, String> {
    call_claude_for_agent_with_type(
        home_dir, registry, agent_id, prompt,
        crate::cost_telemetry::RequestType::Chat,
    ).await
}

/// Like [`call_claude_for_agent`] but allows specifying the request type for telemetry.
///
/// Delegation context (depth, origin, sender) is read from the [`DELEGATION_ENV`]
/// task-local — set by the dispatcher before calling this function.
pub async fn call_claude_for_agent_with_type(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
    request_type: crate::cost_telemetry::RequestType,
) -> Result<String, String> {
    let reg = registry.read().await;

    let agent = if agent_id == "default" {
        reg.main_agent()
    } else {
        reg.get(agent_id)
    };

    let agent = agent.ok_or_else(|| format!("Agent '{agent_id}' not found in registry"))?;

    // Sub-agents inherit a turn id from the dispatch chain via tokio
    // task_local — set by `channel_reply` before invoking the dispatcher.
    // When None (top-level callers, tests), skip citation tracking; we can't
    // pair the citation with a downstream prediction error otherwise.
    //
    // Session id is also propagated so the per-conversation 0.10 cap stays
    // session-scoped across all sub-agent calls within the same channel
    // session (review BLOCKER R2-1).
    let turn_owned = duduclaw_memory::feedback::CURRENT_TURN_ID
        .try_with(|tid| tid.clone())
        .ok()
        .flatten();
    let session_owned = duduclaw_memory::feedback::CURRENT_SESSION_ID
        .try_with(|sid| sid.clone())
        .ok()
        .flatten();
    let citation_ref = turn_owned.as_deref().map(|tid| {
        (agent_id, tid, session_owned.as_deref())
    });
    let system_prompt = build_system_prompt(agent, citation_ref);
    let agent_name = agent.config.agent.name.clone();
    let claude_model = agent.config.model.preferred.clone();
    let fallback_model = agent.config.model.fallback.clone();
    let local_config = agent.config.model.local.clone();
    let api_mode = agent.config.model.api_mode.clone();
    let capabilities = agent.config.capabilities.clone();
    // #15 (2026-05-12) — agent's opt-in to Claude CLI `--bare` mode.
    // Read here so the rest of this function (and the rotator path)
    // can wrap subprocess invocations in a `BARE_MODE` scope and
    // know to filter the rotator to API-key accounts.
    let cli_bare_mode = agent.config.prompt.cli_bare_mode;
    drop(reg);

    // Pending Task Queue is computed from the Task Board so the agent
    // opens each turn aware of its queue. Captured SEPARATELY rather than
    // appended to `system_prompt` because Direct API uses the prompt cache
    // on the system block — appending dynamic content would invalidate
    // the entire cached prefix (Soul/Identity/Skills/Contract) every turn.
    // Direct API path passes this as an uncached secondary system block;
    // CLI / local inference paths concatenate when composing their prompt
    // (those paths manage cache opaquely through the upstream SDK).
    let tasks_suffix = build_pending_tasks_section(home_dir, agent_id).await;

    // Install agent-file-guard PreToolUse hook before any spawn.
    // Blocks the sub-agent from using raw Write/Edit to create
    // agent-structure files outside <home>/agents/<name>/.
    // Best-effort — logs warning on failure and continues.
    let agent_dir = home_dir.join("agents").join(agent_id);

    // RFC-25 Phase 2: when the delegated agent's [runtime] provider is not
    // Claude, route the whole task through the provider-agnostic choke-point
    // (Codex / Gemini / OpenAI-compat). Claude keeps the optimized rotation +
    // local/hybrid path below. This makes sub-agent delegation respect the
    // responding agent's runtime — and is the foundation A2A (Phase 3) builds on.
    // Parse agent.toml once for the routing decision and the choke-point (L7 followup).
    let delegation_settings = crate::runtime_config::load_runtime_settings(&agent_dir);
    if let Some(provider) = delegation_settings.non_claude_provider() {
        info!(
            agent = %agent_id,
            provider = provider.as_str(),
            "delegation: routing through multi-runtime choke-point (non-Claude provider)"
        );
        // RFC-25 A2: non-Claude runtimes have no separate uncached secondary
        // system block (that's a Direct-API cache optimization). Inline the
        // pending-tasks section into the system prompt — the same content and
        // format the Claude CLI / local paths concatenate below — so non-Claude
        // sub-agents still open each turn aware of their Task-Board queue.
        // (`cli_bare_mode` is a Claude-CLI `--bare` flag with no equivalent on
        // other runtimes, so it does not apply here.)
        let system_with_tasks = match &tasks_suffix {
            Some(s) => std::borrow::Cow::Owned(format!("{system_prompt}\n\n---\n\n{s}")),
            None => std::borrow::Cow::Borrowed(system_prompt.as_str()),
        };
        return crate::runtime_dispatch::run_agent_prompt_text(
            crate::runtime_dispatch::AgentPrompt {
                agent_dir: Some(&agent_dir),
                home_dir,
                agent_id,
                prompt,
                system_prompt: &system_with_tasks,
                model: &claude_model,
                max_tokens: 8192,
                provider_override: None,
                // Single-shot by design: delegation / cron / reminder / A2A
                // dispatch a discrete task prompt, not a conversation — the same
                // is true for the Claude path here and the Direct-API path
                // (see the `&[]` at try_direct_api), so this is symmetric across
                // providers, not a non-Claude amnesia gap. Multi-turn history is
                // a channel-reply concept (where a session exists) and is wired
                // there (A1).
                conversation_history: &[],
                request_type: crate::cost_telemetry::RequestType::Dispatch,
                runtime_settings: Some(&delegation_settings),
            },
        )
        .await;
    }

    if agent_dir.exists() {
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) = crate::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await {
            warn!(
                agent = %agent_name,
                error = %e,
                "Failed to install agent-file-guard hook — continuing without enforcement"
            );
        }

        // Phase 3.C.5 (2026-05-14): dispatcher PTY short-circuit.
        //
        // When the agent opts in to `[runtime] pty_pool_enabled = true`,
        // dispatcher-side invocations short-circuit local offload + hybrid
        // routing and go straight to the PTY pool. The semantic is "I've
        // chosen PTY-as-runtime; respect that across all entry points
        // (channel reply + sub-agent dispatch)".
        //
        // Cost gates (local offload, model fallback) are intentionally
        // bypassed because:
        // 1. The operator's intent is clear from the flag.
        // 2. PTY interactive mode reuses sessions across turns, so the
        //    cost saving from local offload is less material.
        // 3. Mixing PTY-with-local-offload would create surprising
        //    behaviour — the in-session conversation context would get
        //    truncated by occasional local-offload diversions.
        let runtime_mode = crate::pty_runtime::runtime_mode_for_agent(&agent_dir);
        if runtime_mode == crate::pty_runtime::RuntimeMode::PtyPool {
            info!(
                agent = %agent_name,
                mode = runtime_mode.as_str(),
                "dispatcher: short-circuit through PTY pool (skipping local offload + hybrid routing)"
            );
            let deadline = std::time::Duration::from_secs(180);
            // Round 4 deferred-cleanup (LOW F-3): canonical options entry.
            // Unbind from hardcoded Claude: the PtyPool kind follows the agent's
            // configured provider. Non-Claude providers are short-circuited to
            // `runtime_dispatch` above (the `non_claude_provider` guard), so this
            // resolves to Claude in practice today — but the coupling is gone.
            let cli_kind = crate::pty_runtime::cli_kind_for_provider(
                delegation_settings.provider,
            )
            .unwrap_or(duduclaw_cli_runtime::CliKind::Claude);
            let acquire = crate::pty_runtime::AcquireOptions::new(
                agent_id,
                cli_kind,
                cli_bare_mode,
            );
            return crate::pty_runtime::acquire_and_invoke_with(
                crate::pty_runtime::InvokeOptions::new(acquire, prompt, deadline),
            )
            .await;
        }
    }

    // For CLI / local inference paths, tasks suffix is inlined into the
    // system prompt — those paths don't use our manual `cache_control`,
    // so an inline append costs nothing cache-wise. For the Direct API
    // path we instead pass the suffix as a separate uncached block.
    let system_prompt_inlined: std::borrow::Cow<str> = match &tasks_suffix {
        Some(s) => std::borrow::Cow::Owned(format!("{system_prompt}\n\n---\n\n{s}")),
        None => std::borrow::Cow::Borrowed(system_prompt.as_str()),
    };

    // P0 fix: global mode gate BEFORE per-agent routing
    let inference_mode = get_inference_mode(home_dir).await;
    match inference_mode.as_str() {
        "local" => {
            // Force local inference regardless of per-agent prefer_local
            let model_id = local_config.as_ref().map(|c| c.model.as_str());
            return call_local_inference(home_dir, prompt, &system_prompt_inlined, model_id)
                .await
                .map_err(|e| format!(
                    "Agent '{agent_name}' is in local-only mode but inference failed: {e}. \
                     Fix local model setup or switch to 'hybrid' mode in config.toml."
                ));
        }
        "claude" => {
            // Skip local entirely, go straight to Claude API
            info!(agent = %agent_name, model = %claude_model, "Claude-only mode");
            let wd = effective_work_dir(&agent_dir);
            let primary_result = call_with_rotation(
                home_dir, agent_id, prompt, &claude_model, &system_prompt_inlined,
                request_type, Some(&capabilities), wd.as_deref(), cli_bare_mode,
            ).await;
            return match primary_result {
                Ok(text) => Ok(text),
                Err(ref e) if is_llm_fallback_error(e) && should_attempt_model_fallback(&claude_model, &fallback_model) => {
                    warn!(
                        primary = %claude_model,
                        fallback = %fallback_model,
                        error = %e,
                        "LLM timeout/overloaded — attempting model fallback (claude mode)"
                    );
                    emit_llm_fallback_audit(home_dir, agent_id, &claude_model, &fallback_model, e).await;
                    call_with_rotation(
                        home_dir, agent_id, prompt, &fallback_model, &system_prompt_inlined,
                        request_type, Some(&capabilities), wd.as_deref(), cli_bare_mode,
                    ).await.map_err(|fe| format_fallback_error_message(&claude_model, e, &fallback_model, &fe))
                }
                Err(e) => Err(e),
            };
        }
        _ => {
            // "hybrid" — SDK-first design (see routing logic below)
        }
    }

    // ══════════════════════════════════════════════════════════════
    // Hybrid mode routing — SDK is the brain, local is cost-saving offload
    //
    // Design principle: "Claude Code SDK = brain, DuDuClaw = plumbing"
    // OAuth subscription is the primary fuel, API Key is the reserve tank.
    //
    //  ① Local offload: Router-confirmed simple queries → zero cost
    //  ② CLI (claude -p): primary brain, uses OAuth subscription
    //     - Multiple OAuth accounts rotated via CLAUDE_CODE_OAUTH_TOKEN
    //  ③ Direct API (API Key): fallback when all OAuth accounts rate-limited
    //     - cache_control for 95%+ cache hit rate
    // ══════════════════════════════════════════════════════════════

    // Validate api_mode
    if !matches!(api_mode.as_str(), "cli" | "direct" | "auto") {
        warn!(
            agent = %agent_name,
            api_mode = %api_mode,
            "Unrecognized api_mode in agent.toml — expected cli/direct/auto, defaulting to cli"
        );
    }

    // ── ① Local offload: only for clearly simple queries ─────────
    let adaptive_prefer = crate::cost_telemetry::should_prefer_local(agent_id).await;
    if let Some(ref local) = local_config {
        let should_try_local = adaptive_prefer || local.use_router || local.prefer_local;
        if should_try_local {
            let reason = if adaptive_prefer { "adaptive-override" }
                else if local.use_router { "router-driven" }
                else { "prefer-local" };
            info!(agent = %agent_name, local_model = %local.model, reason, "Trying local offload");
            match call_local_inference(home_dir, prompt, &system_prompt_inlined, Some(&local.model)).await {
                Ok(response) => {
                    info!(agent = %agent_name, "Query served by local model (cost saved)");
                    return Ok(response);
                }
                Err(e) if e == "ROUTER_ESCALATE_TO_CLOUD" => {
                    info!(agent = %agent_name, "Router: query too complex → escalating to SDK");
                }
                Err(e) => {
                    warn!(agent = %agent_name, error = %e, "Local offload failed → escalating to SDK");
                }
            }
        }
    }

    // ── ② CLI: primary brain (OAuth subscription) ────────────────
    // In "auto" mode: try CLI first. Only fall through to Direct API
    // if CLI fails with rate limit (all OAuth accounts exhausted).
    // In "cli" mode: CLI is the only cloud path.
    // In "direct" mode: skip CLI, go straight to Direct API.
    let wd = effective_work_dir(&agent_dir);
    if api_mode != "direct" {
        info!(agent = %agent_name, model = %claude_model, "Calling Claude CLI (SDK primary)");
        match call_with_rotation(
            home_dir, agent_id, prompt, &claude_model, &system_prompt_inlined, request_type,
            Some(&capabilities), wd.as_deref(), cli_bare_mode,
        ).await {
            Ok(text) => return Ok(text),
            Err(e) => {
                let is_rate = is_rate_limit_error(&e);
                let is_fallback_trigger = is_llm_fallback_error(&e);
                let can_model_fallback = is_fallback_trigger
                    && should_attempt_model_fallback(&claude_model, &fallback_model);

                if can_model_fallback {
                    // Model-level fallback takes priority over account-level
                    // Direct API fallback: switching to a lighter model reuses
                    // existing OAuth accounts and avoids consuming API Key quota.
                    // Even if the error was also a rate-limit, haiku is less
                    // likely to be overloaded and shares the same account pool.
                    warn!(
                        primary = %claude_model,
                        fallback = %fallback_model,
                        error = %e,
                        "LLM timeout/overloaded — attempting model fallback via CLI (hybrid mode)"
                    );
                    emit_llm_fallback_audit(home_dir, agent_id, &claude_model, &fallback_model, &e).await;
                    return call_with_rotation(
                        home_dir, agent_id, prompt, &fallback_model, &system_prompt_inlined,
                        request_type, Some(&capabilities), wd.as_deref(), cli_bare_mode,
                    ).await.map_err(|fe| format_fallback_error_message(&claude_model, &e, &fallback_model, &fe));
                } else if api_mode == "auto" && is_rate {
                    // No model fallback available: all OAuth accounts rate-limited
                    // and the two models are the same (or fallback is unset).
                    // Fall through to Direct API (account-level fallback).
                    warn!(agent = %agent_name, "All CLI accounts rate-limited → trying Direct API fallback");
                } else {
                    // "cli" mode or non-retriable error → report error
                    return Err(e);
                }
            }
        }
    }

    // ── ③ Direct API: fallback with API Key (cache-optimized) ────
    // Only reached when: api_mode="direct", or api_mode="auto" + all OAuth rate-limited.
    // Pass tasks_suffix as a separate uncached block so the static system
    // prefix stays cacheable.
    info!(agent = %agent_name, model = %claude_model, "Trying Direct API (API Key fallback)");
    match try_direct_api(
        home_dir, agent_id, prompt, &claude_model, &system_prompt,
        tasks_suffix.as_deref(), request_type,
    ).await {
        Ok(text) => Ok(text),
        Err(ref e) if is_llm_fallback_error(e) && should_attempt_model_fallback(&claude_model, &fallback_model) => {
            warn!(
                primary = %claude_model,
                fallback = %fallback_model,
                error = %e,
                "LLM Direct API timeout/overloaded — attempting model fallback"
            );
            emit_llm_fallback_audit(home_dir, agent_id, &claude_model, &fallback_model, e).await;
            try_direct_api(
                home_dir, agent_id, prompt, &fallback_model, &system_prompt,
                tasks_suffix.as_deref(), request_type,
            ).await
            .map_err(|fe| format_fallback_error_message(&claude_model, e, &fallback_model, &fe))
        }
        Err(e) => Err(e),
    }
}

/// Check whether an error string indicates a billing/credit exhaustion issue.
///
/// These errors should NOT be retried with the same account — the account
/// needs a long cooldown (topped up manually).
pub(crate) fn is_billing_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("credit")
        || lower.contains("balance")
        || lower.contains("billing")
        || lower.contains("payment")
        || lower.contains("402")
        || lower.contains("insufficient_quota")
}

/// Check whether an error indicates rate limiting (usage limit exhausted).
pub(crate) fn is_rate_limit_error(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("rate limit")
        || lower.contains("rate-limit")
        || lower.contains("ratelimit")
        || lower.contains("429")
        || lower.contains("usage limit")
        || lower.contains("overloaded")
        || lower.contains("capacity limit")
}

/// Where a Direct-API request for a model is served.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectApiRoute {
    /// api.anthropic.com via `crate::direct_api` (ANTHROPIC key, layered
    /// cache breakpoints + invalidation attribution) — the pre-existing path.
    LegacyAnthropic,
    /// A duduclaw-llm provider client, keyed by registry provider id
    /// ("openai", "gemini", "deepseek", ...).
    LlmProvider(String),
}

/// Pure routing decision: registry-known non-Anthropic models go to the
/// matching duduclaw-llm provider; Anthropic models AND unknown models keep
/// the legacy path (unknown → legacy preserves pre-multi-provider behavior
/// exactly, including its failure mode).
fn direct_api_route(model: &str) -> DirectApiRoute {
    match crate::cost_telemetry::model_registry().get(model) {
        Some(info) if info.provider != "anthropic" => {
            DirectApiRoute::LlmProvider(info.provider.clone())
        }
        _ => DirectApiRoute::LegacyAnthropic,
    }
}

/// Build a duduclaw-llm provider client for `provider_id`, resolving the API
/// key from the provider's standard env vars. No key → Err, so callers fall
/// back to the CLI/rotation path exactly like the legacy "no API key" case.
fn build_llm_provider(
    provider_id: &str,
) -> Result<Box<dyn duduclaw_llm::ChatProvider>, String> {
    let key = duduclaw_llm::resolve_env_key(provider_id).ok_or_else(|| {
        format!(
            "no API key for provider {provider_id} — set its standard env var \
             (e.g. OPENAI_API_KEY / GEMINI_API_KEY / DEEPSEEK_API_KEY)"
        )
    })?;
    let auth = duduclaw_llm::ApiAuth::new(key);
    match provider_id {
        "openai" => Ok(Box::new(duduclaw_llm::providers::OpenAiProvider::new(auth))),
        "gemini" | "google" => Ok(Box::new(duduclaw_llm::providers::GeminiProvider::new(auth))),
        other => duduclaw_llm::providers::OpenAiCompatProvider::from_preset(other, auth)
            .map(|p| Box::new(p) as Box<dyn duduclaw_llm::ChatProvider>)
            // Fail closed: no preset → no guessed base URL.
            .ok_or_else(|| format!("no direct-API preset for provider {other}")),
    }
}

/// Build the normalized ChatRequest for a non-Anthropic direct call.
///
/// The system prompt splits on `CACHE_SPLIT_MARKER` — duduclaw-llm re-exports
/// the identical constant as `direct_api.rs` (test-pinned below) — so the
/// marker never reaches the wire. Providers with explicit caching get
/// `Explicit` hints per block; others get plain blocks (their implicit prefix
/// caching ignores hints). `dynamic_system_suffix` lands as a final uncached
/// block so the static prefix stays cache-stable, mirroring the legacy path.
fn build_llm_chat_request(
    model: &str,
    supports_caching: bool,
    system_prompt: &str,
    dynamic_system_suffix: Option<&str>,
    prompt: &str,
) -> duduclaw_llm::ChatRequest {
    use duduclaw_llm::{ChatMessage, ChatRequest, SystemBlock, CACHE_SPLIT_MARKER};

    let mut req = ChatRequest::new(model);
    for segment in system_prompt.split(CACHE_SPLIT_MARKER) {
        let text = segment.trim();
        if text.is_empty() {
            continue;
        }
        req.system.push(if supports_caching {
            SystemBlock::cached(text)
        } else {
            SystemBlock::uncached(text)
        });
    }
    if let Some(suffix) = dynamic_system_suffix {
        let text = suffix.trim();
        if !text.is_empty() {
            req.system.push(SystemBlock::uncached(text));
        }
    }
    req.messages.push(ChatMessage::user(prompt));
    // ChatRequest::new defaults to 4096 == direct_api::DEFAULT_MAX_TOKENS;
    // pinned explicitly so the two paths can't drift apart silently.
    req.max_tokens = 4096;
    req
}

/// Direct API call through a duduclaw-llm provider (non-Anthropic models).
async fn try_llm_provider_api(
    agent_id: &str,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    dynamic_system_suffix: Option<&str>,
    request_type: crate::cost_telemetry::RequestType,
    provider_id: &str,
) -> Result<String, String> {
    let provider = build_llm_provider(provider_id)?;
    let supports_caching = crate::cost_telemetry::model_registry()
        .supports(model, duduclaw_llm::Feature::Caching);
    let req =
        build_llm_chat_request(model, supports_caching, system_prompt, dynamic_system_suffix, prompt);

    info!(provider = provider_id, model, "Trying Direct API via duduclaw-llm provider");
    let resp = provider
        .complete(&req)
        .await
        .map_err(|e| format!("{provider_id} direct API error: {e}"))?;

    // Reasoning fold: TokenUsage has no reasoning field, and NormalizedUsage
    // guarantees `output_tokens + reasoning_tokens` == total billable output
    // (providers that report reasoning inside output set reasoning to 0), so
    // folding here bills reasoning exactly once at the output rate.
    let usage = crate::cost_telemetry::TokenUsage {
        input_tokens: resp.usage.input_tokens,
        cache_read_tokens: resp.usage.cache_read_tokens,
        cache_creation_tokens: resp.usage.cache_write_tokens,
        output_tokens: resp.usage.output_tokens + resp.usage.reasoning_tokens,
    };
    if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
        telemetry.record(agent_id, request_type, model, &usage).await;
    }

    Ok(resp.text())
}

/// Try calling the model's provider API directly (bypassing Claude CLI).
///
/// Anthropic (and registry-unknown) models keep the original
/// `crate::direct_api` path with its cache attribution; registry-known
/// non-Anthropic models route through the matching duduclaw-llm provider.
/// Only works with API key accounts (not OAuth). If no API key is available,
/// returns an error so the caller can fall back to CLI.
async fn try_direct_api(
    home_dir: &Path,
    agent_id: &str,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    dynamic_system_suffix: Option<&str>,
    request_type: crate::cost_telemetry::RequestType,
) -> Result<String, String> {
    if let DirectApiRoute::LlmProvider(provider_id) = direct_api_route(model) {
        return try_llm_provider_api(
            agent_id, prompt, model, system_prompt, dynamic_system_suffix,
            request_type, &provider_id,
        )
        .await;
    }

    let api_key = get_api_key(home_dir).await;
    if api_key.is_empty() {
        return Err("No API key available for Direct API (OAuth accounts require CLI path)".to_string());
    }

    // TODO: pass conversation_history from the caller to enable multi-turn
    // for the Direct API fallback path (currently stateless).
    let scope = format!("{agent_id}:{model}");
    let response = crate::direct_api::call_direct_api_attributed(
        Some(&scope), &api_key, model, system_prompt, dynamic_system_suffix, prompt, &[],
    ).await?;

    // Record telemetry
    if let Some(ref usage) = response.usage {
        if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
            telemetry.record(agent_id, request_type, model, usage).await;
        }
    }

    Ok(response.text)
}

/// Cached inference_mode — avoids reading config.toml on every call (P1-3).
static INFERENCE_MODE_CACHE: std::sync::OnceLock<tokio::sync::RwLock<Option<(std::time::Instant, String)>>> = std::sync::OnceLock::new();

async fn get_inference_mode(home_dir: &Path) -> String {
    let cache = INFERENCE_MODE_CACHE.get_or_init(|| tokio::sync::RwLock::new(None));
    let ttl = std::time::Duration::from_secs(300); // 5 min

    {
        let guard = cache.read().await;
        if let Some((created, mode)) = guard.as_ref() {
            if created.elapsed() < ttl {
                return mode.clone();
            }
        }
    }

    let config_path = home_dir.join("config.toml");
    let content = tokio::fs::read_to_string(&config_path).await.unwrap_or_default();
    let table: toml::Table = content.parse().unwrap_or_default();
    let mode = table.get("general")
        .and_then(|g| g.get("inference_mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("hybrid")
        .to_string();

    *cache.write().await = Some((std::time::Instant::now(), mode.clone()));
    mode
}

/// Cached AccountRotator — avoids rebuilding on every call (BE-H4).
static ROTATOR_CACHE: std::sync::OnceLock<tokio::sync::RwLock<Option<(std::time::Instant, std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>)>>> = std::sync::OnceLock::new();

/// Mutex protecting rotator rebuild — prevents concurrent `claude auth status` subprocesses.
static ROTATOR_INIT_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// Cached InferenceEngine — singleton for local LLM inference.
static INFERENCE_ENGINE: std::sync::OnceLock<tokio::sync::RwLock<Option<std::sync::Arc<duduclaw_inference::InferenceEngine>>>> = std::sync::OnceLock::new();

/// Mutex protecting the one-time initialization of the inference engine.
/// Prevents concurrent tasks from each loading a full GGUF model (OOM risk).
static INFERENCE_INIT_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// Process-lifetime negative cache — set to `true` once
/// `InferenceEngine::init` fails in a way that won't recover this run
/// (e.g. the binary was built without `--features metal`/`cuda`/`vulkan`,
/// so llama.cpp has no backend; or the router is disabled and there's
/// no remote endpoint configured). Every later `get_inference_engine`
/// call short-circuits silently to `None` instead of retrying init and
/// re-emitting the same WARN. Reset is by restarting the gateway —
/// which is also when the operator would have rebuilt with features.
///
/// Before this cache, every channel/dispatch call hit the init path and
/// logged the same "Backend unavailable: llama.cpp — Build with
/// --features metal, cuda, or vulkan" WARN, flooding the gateway log.
static INFERENCE_UNAVAILABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Get or create the inference engine singleton.
async fn get_inference_engine(home_dir: &std::path::Path) -> Option<std::sync::Arc<duduclaw_inference::InferenceEngine>> {
    // Negative-cache fast path: a previous init attempt already failed
    // in a way that won't recover without a gateway restart. Skip silently.
    if INFERENCE_UNAVAILABLE.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }

    let cache = INFERENCE_ENGINE.get_or_init(|| tokio::sync::RwLock::new(None));

    // Fast path: engine already initialized
    {
        let guard = cache.read().await;
        if let Some(engine) = guard.as_ref() {
            return Some(engine.clone());
        }
    }

    // Slow path: serialize initialization to prevent concurrent model loading
    let init_lock = INFERENCE_INIT_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _init_guard = init_lock.lock().await;

    // Double-check after acquiring lock (another task may have initialized
    // or marked the engine permanently unavailable).
    if INFERENCE_UNAVAILABLE.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    {
        let guard = cache.read().await;
        if let Some(engine) = guard.as_ref() {
            return Some(engine.clone());
        }
    }

    // Initialize engine
    let engine = duduclaw_inference::InferenceEngine::new(home_dir).await;
    if let Err(e) = engine.init().await {
        // One-shot WARN: record the failure, latch the negative cache, and
        // fall through to SDK for the rest of this process's lifetime.
        warn!(
            error = %e,
            "Failed to initialize inference engine — disabling local offload for this process (build with --features metal/cuda/vulkan to enable llama.cpp, or configure [openai_compat] in inference.toml for a remote backend)"
        );
        INFERENCE_UNAVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        return None;
    }
    if !engine.is_available().await {
        warn!(
            "Inference engine initialized but reports no available backend — disabling local offload for this process"
        );
        INFERENCE_UNAVAILABLE.store(true, std::sync::atomic::Ordering::Relaxed);
        return None;
    }
    let arc = std::sync::Arc::new(engine);
    *cache.write().await = Some(arc.clone());
    Some(arc)
}

/// Call local inference engine instead of Claude CLI.
///
/// If the confidence router is enabled, it may decide to escalate to Cloud API
/// (returns `Err` with a special marker so the caller knows to try Cloud).
///
/// Public wrapper for channel_reply fallback chain.
pub async fn try_local_inference(
    home_dir: &std::path::Path,
    prompt: &str,
    system_prompt: &str,
    model_id: Option<&str>,
) -> Result<String, String> {
    call_local_inference(home_dir, prompt, system_prompt, model_id).await
}

async fn call_local_inference(
    home_dir: &std::path::Path,
    prompt: &str,
    system_prompt: &str,
    model_id: Option<&str>,
) -> Result<String, String> {
    let engine = get_inference_engine(home_dir)
        .await
        .ok_or_else(|| "Local inference engine not available".to_string())?;

    let request = duduclaw_inference::InferenceRequest {
        system_prompt: system_prompt.to_string(),
        user_prompt: prompt.to_string(),
        params: engine.config().generation.clone(),
        model_id: model_id.map(|s| s.to_string()),
    };

    // Use router if enabled — may escalate to Cloud API
    if engine.router_enabled() {
        match engine.route_and_generate(&request).await {
            Ok(Some(response)) => {
                info!(
                    model = %response.model_id,
                    tokens = response.tokens_generated,
                    tps = format!("{:.1}", response.tokens_per_second),
                    ms = response.generation_time_ms,
                    "Local inference completed (routed)"
                );
                return Ok(response.text);
            }
            Ok(None) => {
                // Router decided Cloud API is needed
                return Err("ROUTER_ESCALATE_TO_CLOUD".to_string());
            }
            Err(e) => {
                warn!(error = %e, "Routed local inference failed");
                return Err(format!("Local inference error: {e}"));
            }
        }
    }

    // No router — direct generation
    let response = engine
        .generate(&request)
        .await
        .map_err(|e| format!("Local inference error: {e}"))?;

    info!(
        model = %response.model_id,
        tokens = response.tokens_generated,
        tps = format!("{:.1}", response.tokens_per_second),
        ms = response.generation_time_ms,
        "Local inference completed"
    );

    Ok(response.text)
}

/// Get or create a cached AccountRotator (refreshes every 5 minutes).
/// Public accessor for the cached rotator — used by handlers.rs too.
pub async fn get_rotator_cached(home_dir: &Path) -> Result<std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>, String> {
    get_rotator(home_dir).await
}

/// Drop the cached `AccountRotator` so the next `get_rotator_cached` rebuilds
/// from the current `config.toml`. Call after mutating accounts (e.g. a
/// one-click login that just added an OAuth token) so the dashboard reflects it
/// immediately instead of after the 5-minute TTL.
pub async fn invalidate_rotator_cache() {
    if let Some(cache) = ROTATOR_CACHE.get() {
        *cache.write().await = None;
    }
}

async fn get_rotator(home_dir: &Path) -> Result<std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator>, String> {
    let cache = ROTATOR_CACHE.get_or_init(|| tokio::sync::RwLock::new(None));
    let ttl = std::time::Duration::from_secs(300); // 5 min cache

    // Check if cached version is still valid
    {
        let guard = cache.read().await;
        if let Some((created, rotator)) = guard.as_ref() {
            if created.elapsed() < ttl {
                return Ok(rotator.clone());
            }
        }
    }

    // Serialize rebuild to prevent concurrent `claude auth status` subprocesses
    let init_lock = ROTATOR_INIT_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _init_guard = init_lock.lock().await;

    // Double-check after acquiring lock (another task may have rebuilt)
    {
        let guard = cache.read().await;
        if let Some((created, rotator)) = guard.as_ref() {
            if created.elapsed() < ttl {
                return Ok(rotator.clone());
            }
        }
    }

    // Rebuild
    let config_content = tokio::fs::read_to_string(home_dir.join("config.toml"))
        .await
        .unwrap_or_default();
    let config_table: toml::Table = config_content.parse().unwrap_or_default();
    let rotator = duduclaw_agent::account_rotator::create_from_config(&config_table);
    rotator.load_from_config(home_dir).await?;
    let arc = std::sync::Arc::new(rotator);
    *cache.write().await = Some((std::time::Instant::now(), arc.clone()));
    Ok(arc)
}

/// Spawn a background task that periodically probes unhealthy accounts and
/// restores them when they recover. This ensures that rate-limited or
/// temporarily failed accounts are automatically brought back online
/// according to their priority, without waiting for the next user request.
///
/// Runs every `interval_secs` (default: 60 seconds from config.toml
/// `[rotation].health_check_interval_seconds`).
pub fn spawn_health_probe(home_dir: PathBuf, interval_secs: u64) {
    let interval = std::time::Duration::from_secs(interval_secs.max(10));
    tokio::spawn(async move {
        // Wait a bit before first probe — let the system fully boot
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;

        loop {
            tokio::time::sleep(interval).await;

            let rotator = match get_rotator(&home_dir).await {
                Ok(r) => r,
                Err(_) => continue,
            };

            let restored = rotator.probe_and_restore().await;
            if restored > 0 {
                info!(restored, "Health probe restored accounts");
            }
        }
    });
}

/// Call Claude CLI with account rotation — tries next account on failure.
///
/// Records token usage telemetry when available.
async fn call_with_rotation(
    home_dir: &Path,
    agent_id: &str,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    request_type: crate::cost_telemetry::RequestType,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    work_dir: Option<&Path>,
    bare_mode: bool,
) -> Result<String, String> {
    // Pre-flight: check 200K price cliff
    if let Some(estimated) = crate::cost_telemetry::check_price_cliff(system_prompt, prompt) {
        warn!(
            agent_id,
            estimated_tokens = estimated,
            "WARNING: Estimated input tokens near 200K price cliff — pricing will double"
        );
    }

    let rotator = get_rotator(home_dir).await?;

    // Fresh-install passthrough: no accounts configured → fall back to ambient
    // env (user's default `claude auth login` session). Matches the same guard
    // in `call_claude_cli_rotated` so both paths behave identically.
    if rotator.count().await == 0 {
        if bare_mode {
            // `--bare` strips ambient OAuth, so a fresh install with no
            // rotator accounts and bare_mode opted-in can't possibly work.
            // Fail loud here rather than producing a "Not logged in"
            // error from the subprocess.
            return Err(format!(
                "agent {agent_id} has `[prompt] cli_bare_mode = true` but no \
                 accounts are configured in the rotator. Add an API-key \
                 account or remove the bare_mode flag."
            ));
        }
        info!(agent_id, "No rotator accounts — using ambient env fallback");
        let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let resp = call_claude_with_env(prompt, model, system_prompt, &empty, capabilities, work_dir).await?;

        if let Some(ref usage) = resp.usage {
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                telemetry.record(agent_id, request_type, model, usage).await;
            }
        }
        return Ok(resp.text);
    }

    let max_attempts = rotator.count().await.max(1);
    let mut last_error = String::new();

    for attempt in 0..max_attempts {
        let selected = match rotator.select().await {
            Some(s) => s,
            None => break,
        };

        // #15 (2026-05-12): bare_mode requires ANTHROPIC_API_KEY auth.
        // OAuth accounts can't supply that, so skip them with a hint
        // rather than waste a subprocess on a guaranteed "Not logged in"
        // failure. Falls through to the next account.
        if bare_mode
            && selected.auth_method
                == duduclaw_agent::account_rotator::AuthMethod::OAuth
        {
            warn!(
                account = %selected.id,
                "skipping OAuth account — agent requested cli_bare_mode \
                 which requires ANTHROPIC_API_KEY auth"
            );
            continue;
        }

        info!(account = %selected.id, method = ?selected.auth_method, attempt, bare_mode, "Trying account");

        let bare_scope = BARE_MODE.scope(
            bare_mode,
            call_claude_with_env(prompt, model, system_prompt, &selected.env_vars, capabilities, work_dir),
        );
        match bare_scope.await {
            Ok(response) => {
                // Use telemetry-based cost if usage available, else rough estimate
                let cost = if let Some(ref usage) = response.usage {
                    if selected.auth_method == duduclaw_agent::account_rotator::AuthMethod::OAuth {
                        0
                    } else {
                        // Registry-aware (falls back to legacy Sonnet rates for
                        // unknown models) — same unit as monthly_budget_cents.
                        crate::cost_telemetry::cost_for(model, usage)
                    }
                } else if selected.auth_method == duduclaw_agent::account_rotator::AuthMethod::OAuth {
                    0
                } else {
                    ((prompt.len() + response.text.len()) / 1000).max(1) as u64
                };
                rotator.on_success(&selected.id, cost).await;

                // Record telemetry
                if let Some(ref usage) = response.usage {
                    if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                        telemetry.record(agent_id, request_type, model, usage).await;
                    }
                }

                return Ok(response.text);
            }
            Err(e) => {
                last_error = e.clone();
                if is_billing_error(&e) {
                    // Billing/credit exhaustion: long cooldown (24h), mark unhealthy immediately
                    warn!(account = %selected.id, error = %e, "Account billing exhausted — 24h cooldown");
                    rotator.on_billing_exhausted(&selected.id).await;
                } else if is_rate_limit_error(&e) {
                    rotator.on_rate_limited(&selected.id).await;
                } else {
                    rotator.on_error(&selected.id).await;
                }
                warn!(account = %selected.id, error = %e, "Account failed, trying next");
            }
        }
    }

    // All rotated accounts failed.
    // Note: the AccountRotator already includes env-var and [api]-section keys
    // as accounts, so retrying with get_api_key() here would be redundant.
    Err(format!("All accounts exhausted. Last error: {last_error}"))
}

/// Public API key getter for use by other modules (e.g., sandbox dispatcher).
pub async fn get_api_key_from_home(home_dir: &Path) -> String {
    get_api_key(home_dir).await
}

/// Get the API key from env var or config.toml.
async fn get_api_key(home_dir: &Path) -> String {
    // Environment variable takes precedence
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return key;
        }
    }
    // Use shared encrypted config reader (tries _enc first, falls back to plaintext)
    crate::config_crypto::read_encrypted_config_field(home_dir, "api", "anthropic_api_key")
        .await
        .unwrap_or_default()
}

/// Hard max timeout — absolute safety net to kill truly hung processes.
const HARD_MAX_TIMEOUT_SECS: u64 = 30 * 60; // 30 minutes

/// Response from a Claude CLI call, including optional token usage telemetry.
struct ClaudeResponse {
    text: String,
    usage: Option<crate::cost_telemetry::TokenUsage>,
}

/// Spawn a `claude` CLI process with streaming output and read the result.
///
/// Uses `--output-format stream-json --verbose`. No idle timeout — the process
/// runs until it completes or hits the hard max timeout (30 min safety net).
/// An optional `on_progress` callback receives `ProgressEvent`s for keepalive
/// and tool-use progress (used by channel reply; cron/dispatch pass `None`).
///
/// Extracts `TokenUsage` from the `result` event's `usage` field when available.
async fn call_claude_streaming(
    cmd: &mut tokio::process::Command,
    on_progress: Option<&crate::channel_reply::ProgressCallback>,
) -> Result<ClaudeResponse, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("claude CLI spawn error: {e}"))?;
    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let mut reader = BufReader::new(stdout).lines();

    // Drain stderr asynchronously to prevent pipe buffer deadlock.
    // Without this, if claude CLI writes >64KB to stderr (common in verbose
    // mode), the pipe fills up and the child blocks forever.
    let stderr = child.stderr.take();
    tokio::spawn(async move {
        if let Some(e) = stderr {
            let mut lines = BufReader::new(e).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        }
    });

    let mut result_text = String::new();
    let mut token_usage: Option<crate::cost_telemetry::TokenUsage> = None;
    let mut last_tool_reported: Option<String> = None;

    // Keepalive timer (90s) — only meaningful when on_progress is Some
    let mut keepalive = tokio::time::interval(
        std::time::Duration::from_secs(crate::channel_reply::KEEPALIVE_INTERVAL_SECS),
    );
    keepalive.reset();

    // Hard max timeout — absolute safety net
    let hard_deadline = tokio::time::sleep(
        std::time::Duration::from_secs(HARD_MAX_TIMEOUT_SECS),
    );
    tokio::pin!(hard_deadline);

    loop {
        tokio::select! {
            line_result = reader.next_line() => {
                match line_result {
                    Ok(None) => break,
                    Err(e) => {
                        let _ = child.kill().await;
                        return Err(format!("claude CLI read error: {e}"));
                    }
                    Ok(Some(line)) => {
                        keepalive.reset();
                        if line.trim().is_empty() { continue; }

                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                            match event.get("type").and_then(|t| t.as_str()) {
                                Some("result") => {
                                    // Terminal error from stream-json — promote to Err
                                    // so the caller (rotator / classifier) can route it.
                                    // Previously this embedded "[error] ..." into
                                    // result_text which was then returned as Ok,
                                    // silently surfacing CLI errors as the reply.
                                    if event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false) {
                                        let err_text = event
                                            .get("result")
                                            .and_then(|r| r.as_str())
                                            .or_else(|| event.get("error").and_then(|e| e.as_str()))
                                            .unwrap_or("Unknown stream-json error");
                                        let _ = child.kill().await;
                                        return Err(format!(
                                            "claude CLI stream error: {err_text}"
                                        ));
                                    }
                                    if let Some(text) = event.get("result").and_then(|r| r.as_str()) {
                                        if !text.is_empty() {
                                            result_text = text.to_string();
                                        }
                                    }
                                    if let Some(usage_val) = event.get("usage") {
                                        token_usage = crate::cost_telemetry::TokenUsage::from_json(usage_val);
                                    }
                                }
                                Some("assistant") => {
                                    // Envelope-level error field (newer claude-code)
                                    if let Some(err) = event.get("error").and_then(|e| e.as_str()) {
                                        let _ = child.kill().await;
                                        return Err(format!(
                                            "claude CLI assistant error: {err}"
                                        ));
                                    }
                                    if let Some(content) = event.pointer("/message/content").and_then(|c| c.as_array()) {
                                        for block in content {
                                            let block_type = block.get("type").and_then(|t| t.as_str());
                                            match block_type {
                                                Some("text") => {
                                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                        result_text = text.to_string();
                                                    }
                                                }
                                                Some("tool_use") => {
                                                    if let Some(cb) = on_progress {
                                                        let tool = block.get("name")
                                                            .and_then(|n| n.as_str())
                                                            .unwrap_or("unknown")
                                                            .to_string();
                                                        let detail = crate::channel_reply::extract_tool_detail(block);
                                                        let dominated = last_tool_reported
                                                            .as_ref()
                                                            .is_some_and(|prev| *prev == tool && detail.is_none());
                                                        if !dominated {
                                                            cb(crate::channel_reply::ProgressEvent::ToolUse {
                                                                tool: tool.clone(),
                                                                detail,
                                                            });
                                                            last_tool_reported = Some(tool);
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    if token_usage.is_none() {
                                        if let Some(usage_val) = event.pointer("/message/usage") {
                                            token_usage = crate::cost_telemetry::TokenUsage::from_json(usage_val);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            _ = keepalive.tick() => {
                if let Some(cb) = on_progress {
                    cb(crate::channel_reply::ProgressEvent::Keepalive);
                }
            }

            _ = &mut hard_deadline => {
                warn!("claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s) — killing process");
                let _ = child.kill().await;
                return Err(format!(
                    "claude CLI hard timeout ({HARD_MAX_TIMEOUT_SECS}s, partial output: {} chars)",
                    result_text.len()
                ));
            }
        }
    }

    let status = child.wait().await.map_err(|e| format!("wait error: {e}"))?;
    // Any non-zero exit is now a hard failure. Previously we only errored
    // when result_text was empty, which would surface CLI error text as
    // the "reply" whenever the stream-json layer accidentally wrote it.
    if !status.success() {
        return Err(format!(
            "claude CLI exit {} (stream tail: {:?})",
            status.code().unwrap_or(-1),
            result_text.chars().take(120).collect::<String>()
        ));
    }

    let result_text = result_text.trim().to_string();
    if result_text.is_empty() {
        return Err("Empty response from claude CLI".to_string());
    }

    Ok(ClaudeResponse { text: result_text, usage: token_usage })
}

// ── Delegation context (task-local) ──────────────────────────

tokio::task_local! {
    /// Delegation environment injected by the bus dispatcher before calling
    /// Claude CLI.  `prepare_claude_cmd` reads this to set per-subprocess
    /// env vars.  Thread-safe because each dispatch runs in its own
    /// `tokio::spawn` task with its own task-local scope.
    pub static DELEGATION_ENV: std::collections::HashMap<String, String>;

    /// Channel context injected by channel handlers (Telegram, LINE, Discord, etc.)
    /// before spawning a CLI session.  Format: `<channel_type>:<channel_id>[:<thread_id>]`.
    /// The MCP `send_to_agent` tool reads this to register a delegation callback
    /// so the dispatcher can forward sub-agent responses back to the originating channel.
    pub static REPLY_CHANNEL: String;

    /// RFC-22 P1-7: caller agent_id for cost_telemetry attribution along the
    /// channel_reply path. Without this, `spawn_claude_cli_with_env` cannot
    /// record per-agent token usage and `cost_telemetry.db` shows 0 entries
    /// for whichever agent owned the channel reply (5/5 trace had agnes
    /// running 23 minutes with no telemetry row).
    pub static CHANNEL_REPLY_AGENT_ID: String;

    /// Worktree path override injected by the dispatcher when L0 worktree
    /// isolation is enabled.  `prepare_claude_cmd` uses this as the working
    /// directory instead of the agent's base directory.
    pub static WORKTREE_PATH: Option<std::path::PathBuf>;

    /// **#15 (2026-05-12)** — when set to `true`, `prepare_claude_cmd`
    /// adds `--bare` to the spawned `claude` subprocess. This disables
    /// CLAUDE.md auto-discovery (the leak documented in #15's spike)
    /// at the cost of OAuth/keychain auth — the caller must arrange
    /// for `ANTHROPIC_API_KEY` to be present in env_vars.
    ///
    /// Callers should only set this scope when:
    ///   (a) the agent opted in via `[prompt] cli_bare_mode = true`, AND
    ///   (b) an `AuthMethod::ApiKey` account is available in the rotator
    ///       (or the call site otherwise provides an API key).
    ///
    /// Default value is `false`. Out-of-scope reads (i.e. no
    /// `BARE_MODE.scope(...)` wrapping) safely return `false`.
    pub static BARE_MODE: bool;
}

/// Prepare a `claude` CLI command with common args and env vars.
///
/// When `capabilities` is provided, high-risk tools not explicitly enabled
/// are added to `--disallowedTools` (deny-by-default security posture).
fn prepare_claude_cmd(
    claude_path: &str,
    prompt: &str,
    model: &str,
    system_prompt: &str,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    work_dir: Option<&Path>,
) -> (tokio::process::Command, Option<tempfile::TempPath>) {
    let mut cmd = duduclaw_core::platform::async_command_for(claude_path);

    // Set working directory so Claude CLI auto-discovers the agent's
    // .mcp.json and .claude/settings.json from the project root.
    if let Some(dir) = work_dir {
        cmd.current_dir(dir);
    }
    // #15 (2026-05-12) — opt in to `--bare` when the calling site has
    // wrapped this invocation in a `BARE_MODE.scope(true, ...)`. The
    // flag disables CLAUDE.md auto-discovery (the leak from #15's
    // spike) at the cost of OAuth — caller is responsible for setting
    // `ANTHROPIC_API_KEY` in env_vars.
    let bare_mode = BARE_MODE.try_with(|b| *b).unwrap_or(false);
    if bare_mode {
        cmd.arg("--bare");
    }

    cmd.args([
        "-p", prompt,
        "--model", model,
        "--output-format", "stream-json",
        "--verbose",
        // Subprocess has no TTY — auto-accept tool permissions.
        // Security is enforced by DuDuClaw's CONTRACT.toml + container sandbox.
        "--permission-mode", "auto",
        // Auto-approve all DuDuClaw MCP tools + a curated set of native Claude
        // Code tools. When `--allowedTools` is specified, Claude Code treats
        // it as the **only** auto-approved list — anything else would need
        // interactive confirmation, which is impossible in `-p` subprocess
        // mode and causes the tool to silently no-op / return empty.
        //
        // Prior to v1.8.30 only `mcp__duduclaw__*` was listed, which meant
        // `WebSearch` / `WebFetch` (Anthropic server-side) silently returned
        // 0 results for cron researcher agents even though they work fine in
        // interactive Claude Code. The allowlist is applied below so a
        // per-agent `allowed_tools` override (HS12) can narrow it.
        // Allow enough agentic turns for complex tasks (read → think → write).
        "--max-turns", "50",
    ]);

    // Apply tool restrictions based on agent capabilities (deny-by-default)
    let caps = capabilities.cloned().unwrap_or_default();

    // HS12: honor a per-agent `allowed_tools` override. When configured, it
    // becomes the ONLY auto-approved set (Claude Code allowlist mode), so an
    // operator can pin a sub-agent to e.g. `["Read"]`. When unset, fall back to
    // the curated default that restores WebSearch/WebFetch research capability.
    const DEFAULT_ALLOWED_TOOLS: &str =
        "mcp__duduclaw__*,WebSearch,WebFetch,Read,Write,Edit,Glob,Grep,Bash,TodoWrite";
    let allowed = caps.allowed_tools();
    let allowed_csv = if allowed.is_empty() {
        DEFAULT_ALLOWED_TOOLS.to_string()
    } else {
        allowed.join(",")
    };
    cmd.args(["--allowedTools", &allowed_csv]);

    let denied = caps.disallowed_tools();
    if !denied.is_empty() {
        let denied_csv = denied.join(",");
        cmd.args(["--disallowedTools", &denied_csv]);
    }

    // Signal bash-gate.sh to allow browser automation commands
    if caps.browser_via_bash {
        cmd.env("DUDUCLAW_BROWSER_VIA_BASH", "1");
    }

    // CACHE_SPLIT_MARKER is a Direct-API-only layering hint — strip it here.
    let system_prompt_cli: std::borrow::Cow<'_, str> =
        if system_prompt.contains(crate::direct_api::CACHE_SPLIT_MARKER) {
            std::borrow::Cow::Owned(system_prompt.replace(crate::direct_api::CACHE_SPLIT_MARKER, ""))
        } else {
            std::borrow::Cow::Borrowed(system_prompt)
        };
    let system_prompt = system_prompt_cli.as_ref();
    let prompt_guard = if !system_prompt.is_empty() {
        match tempfile::NamedTempFile::new() {
            Ok(mut f) => {
                use std::io::Write;
                match f.write_all(system_prompt.as_bytes()) {
                    Ok(()) => {
                        let path = f.into_temp_path();
                        cmd.args(["--system-prompt-file", &path.to_string_lossy()]);
                        Some(path)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to write system prompt tempfile, using arg fallback");
                        cmd.args(["--system-prompt", system_prompt]);
                        None
                    }
                }
            }
            Err(_) => {
                cmd.args(["--system-prompt", system_prompt]);
                None
            }
        }
    } else {
        None
    };

    // Prevent "nested session" error when gateway was launched from a Claude Code session
    cmd.env_remove("CLAUDECODE");

    // Inject delegation context if running inside a dispatcher/cron task.
    // These env vars propagate to the MCP server subprocess so it can
    // enforce depth limits without trusting LLM-supplied tool params.
    match DELEGATION_ENV.try_with(|env| {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }) {
        Ok(()) => { /* delegation context injected */ }
        Err(_) => {
            // Task-local not set — this is normal for regular chat (non-delegation).
            // Delegation depth tracking is not needed for direct user→agent chat.
            debug!("No DELEGATION_ENV task-local — delegation depth tracking inactive");
        }
    }

    // Inject channel reply context so `send_to_agent` MCP tool can register
    // delegation callbacks for sub-agent response forwarding.
    if let Ok(channel) = REPLY_CHANNEL.try_with(|ch| ch.clone()) {
        cmd.env(duduclaw_core::ENV_REPLY_CHANNEL, &channel);
    }

    // v1.10: Inject wiki RL trust feedback context so the MCP server can
    // attach turn_id / session_id to sub-agent dispatch BusMessages.
    // Same pattern as REPLY_CHANNEL — task_local set in channel_reply path,
    // read here, propagated to subprocess via env var.
    if let Ok(Some(turn_id)) =
        duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(|t| t.clone())
    {
        cmd.env(duduclaw_core::ENV_TRUST_TURN_ID, &turn_id);
    }
    if let Ok(Some(session_id)) =
        duduclaw_memory::feedback::CURRENT_SESSION_ID.try_with(|s| s.clone())
    {
        cmd.env(duduclaw_core::ENV_TRUST_SESSION_ID, &session_id);
    }

    (cmd, prompt_guard)
}

/// Call claude CLI with custom env vars (supports both OAuth and API key).
async fn call_claude_with_env(
    prompt: &str,
    model: &str,
    system_prompt: &str,
    env_vars: &std::collections::HashMap<String, String>,
    capabilities: Option<&duduclaw_core::types::CapabilitiesConfig>,
    work_dir: Option<&Path>,
) -> Result<ClaudeResponse, String> {
    let claude = duduclaw_core::which_claude().ok_or("Claude CLI not found")?;
    let (mut cmd, _prompt_guard) = prepare_claude_cmd(&claude, prompt, model, system_prompt, capabilities, work_dir);

    for (key, value) in env_vars {
        if value.is_empty() {
            cmd.env_remove(key);
        } else {
            cmd.env(key, value);
        }
    }

    call_claude_streaming(&mut cmd, None).await
}

// ---------------------------------------------------------------------------
// Tests — Direct-API multi-provider routing (W2)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod direct_api_routing_tests {
    use super::*;
    use duduclaw_llm::{CacheHint, ContentPart, Role};

    #[test]
    fn route_decision_table() {
        // Anthropic models (registry-known) → legacy path.
        assert_eq!(direct_api_route("claude-sonnet-5"), DirectApiRoute::LegacyAnthropic);
        assert_eq!(
            direct_api_route("anthropic/claude-haiku-4-5"),
            DirectApiRoute::LegacyAnthropic
        );
        // Registry-unknown claude id → legacy path (behavior-compatible).
        assert_eq!(direct_api_route("claude-sonnet-4-6"), DirectApiRoute::LegacyAnthropic);
        // Known non-Anthropic models → their duduclaw-llm provider.
        assert_eq!(
            direct_api_route("gpt-5.4"),
            DirectApiRoute::LlmProvider("openai".to_string())
        );
        assert_eq!(
            direct_api_route("gemini-3.1-pro"),
            DirectApiRoute::LlmProvider("gemini".to_string())
        );
        assert_eq!(
            direct_api_route("deepseek-v3.2"),
            DirectApiRoute::LlmProvider("deepseek".to_string())
        );
        assert_eq!(
            direct_api_route("deepseek/deepseek-v3.2"),
            DirectApiRoute::LlmProvider("deepseek".to_string())
        );
        // Unknown model → legacy path (fail-safe, unchanged failure mode).
        assert_eq!(direct_api_route("unknown-model"), DirectApiRoute::LegacyAnthropic);
    }

    /// The two marker constants MUST stay byte-identical — prompt assemblers
    /// write the gateway constant, the llm path splits on the crate constant.
    #[test]
    fn cache_split_markers_stay_in_sync() {
        assert_eq!(duduclaw_llm::CACHE_SPLIT_MARKER, crate::direct_api::CACHE_SPLIT_MARKER);
    }

    #[test]
    fn chat_request_splits_marker_into_cached_blocks_plus_uncached_suffix() {
        let system = format!(
            "# Static\nsoul\n{}\n# Semi\nwiki\n",
            duduclaw_llm::CACHE_SPLIT_MARKER
        );
        let req = build_llm_chat_request(
            "gemini-3.1-pro",
            true,
            &system,
            Some("## Pending Tasks\n- t1"),
            "hello",
        );
        assert_eq!(req.model, "gemini-3.1-pro");
        assert_eq!(req.max_tokens, 4096);
        // 2 split blocks (Explicit) + 1 uncached dynamic suffix.
        assert_eq!(req.system.len(), 3);
        assert_eq!(req.system[0].text, "# Static\nsoul");
        assert_eq!(req.system[0].cache, CacheHint::Explicit);
        assert_eq!(req.system[1].text, "# Semi\nwiki");
        assert_eq!(req.system[1].cache, CacheHint::Explicit);
        assert_eq!(req.system[2].text, "## Pending Tasks\n- t1");
        assert_eq!(req.system[2].cache, CacheHint::None);
        // The marker text never survives into any block.
        assert!(req.system.iter().all(|b| !b.text.contains("cache-split")));
        // Single user message carrying the prompt; no tools.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[0].parts, vec![ContentPart::Text("hello".to_string())]);
        assert!(req.tools.is_empty());
    }

    #[test]
    fn chat_request_without_caching_strips_marker_and_leaves_blocks_uncached() {
        let system = format!("A{}B", duduclaw_llm::CACHE_SPLIT_MARKER);
        let req = build_llm_chat_request("qwen3.7-max", false, &system, None, "hi");
        assert_eq!(req.system.len(), 2);
        assert!(req.system.iter().all(|b| b.cache == CacheHint::None));
        assert!(req.system.iter().all(|b| !b.text.contains("cache-split")));
    }

    #[test]
    fn chat_request_empty_system_and_blank_suffix_produce_no_blocks() {
        let req = build_llm_chat_request("deepseek-v3.2", true, "", Some("   "), "hi");
        assert!(req.system.is_empty());
        assert_eq!(req.messages.len(), 1);
    }
}
