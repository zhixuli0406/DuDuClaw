use super::*;

/// Inner implementation shared by both default-agent and explicit-agent paths.
///
/// When `agent_override` is `Some(name)`, the named agent is looked up directly.
/// When `None`, the default agent resolution logic (config.toml → main_agent) is used.
// OTel GenAI semconv (Development): root `invoke_agent` span for one channel
// turn. Attribute names are centralized in `crate::otel` (tracing macros need
// literal field names, so the dotted literals here mirror those consts).
// Agent/model/usage are resolved mid-flight, so they are declared Empty and
// `Span::record`ed post-hoc (usage in `spawn_claude_cli_with_env`).
#[tracing::instrument(
    name = "invoke_agent",
    skip_all,
    fields(
        gen_ai.operation.name = "invoke_agent",
        gen_ai.system = tracing::field::Empty,
        gen_ai.provider.name = tracing::field::Empty,
        gen_ai.agent.name = tracing::field::Empty,
        gen_ai.request.model = tracing::field::Empty,
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
    )
)]
pub(super) async fn build_reply_with_session_inner(
    text: &str,
    ctx: &ReplyContext,
    agent_override: Option<&str>,
    session_id: &str,
    user_id: &str,
    on_progress: Option<ProgressCallback>,
) -> String {
    // ── User access gate (allowlist / blocklist / pairing) ──
    // Single enforcement point for all channels. Open-by-default: returns
    // None unless the operator configured access settings for this channel.
    if let Some(early_reply) = check_user_access_gate(ctx, session_id, user_id, text).await {
        return early_reply;
    }

    // ── W3-1 `/takeover` lifecycle command (D3) ──
    // Handled before the typing-takeover gate below so that an explicit
    // command always works — including `/takeover end` typed while the
    // conversation is paused, which must not be swallowed as "a manager
    // spoke, refresh the window". Zero LLM cost: it returns before any agent
    // is resolved. Lives here rather than in each channel's command
    // interceptor because it is the only place that carries BOTH the
    // conversation and the sender's channel account id.
    if let Some(tk) = crate::chat_commands::parse_takeover(text) {
        return crate::chat_commands::handle_takeover(ctx, session_id, user_id, &tk).await;
    }

    // ── W3-1 human takeover (D1/D2/D5) ──
    // Placed immediately after the access gate and before any agent
    // resolution / LLM work: a manager typing into this conversation IS the
    // takeover declaration, and while somebody holds the conversation the AI
    // must not merely stop *starting* work — it must not produce a reply at
    // all. `Silent` returns an empty string, which every channel already
    // treats as "send nothing" (the same contract the blocked-user and
    // circuit-breaker paths use); the inbound turn is still recorded in the
    // session so the AI resumes with full context.
    match crate::takeover::intercept(ctx, session_id, user_id, text).await {
        Some(crate::takeover::Intercepted::Announce(msg)) => return msg,
        Some(crate::takeover::Intercepted::Silent) => return String::new(),
        None => {}
    }

    // BLOCKER fix (review B1): use a fresh per-turn ID for citation tracking
    // and prediction-error feedback. `session_id` spans many turns; sharing
    // it as the citation key meant prior turns' citations were attributed
    // to the next turn's prediction error. The session id is still used for
    // session manager / metrics; only the trust feedback path switches.
    let turn_id = format!("{session_id}#{}", uuid::Uuid::new_v4());

    // Determine which agent to use
    let reg = ctx.registry.read().await;
    let agent = if let Some(name) = agent_override {
        // Explicit agent name (per-agent Discord bot)
        reg.get(name).or_else(|| reg.main_agent())
    } else {
        // Resolve via AgentResolver: trigger word → channel binding → default_agent → main_agent
        let channel = session_id
            .split(':')
            .next()
            .unwrap_or("unknown")
            .to_string();
        let msg = Message {
            id: String::new(),
            message_type: MessageType::Incoming,
            channel,
            chat_id: session_id.to_string(),
            sender: user_id.to_string(),
            text: text.to_string(),
            timestamp: chrono::Utc::now(),
            agent_id: None,
        };
        let resolver = AgentResolver::new(&reg);
        if let Some(resolved) = resolver.resolve(&msg) {
            Some(resolved)
        } else {
            // Fallback: config.toml default_agent → main_agent()
            let default_agent_name = get_default_agent(&ctx.home_dir).await;
            if let Some(name) = &default_agent_name {
                match reg.get(name) {
                    Some(a) => Some(a),
                    None => {
                        // A dangling `default_agent` (renamed/removed agent) is
                        // the classic cause of "identity mixing": routing
                        // silently falls back to an arbitrary main agent, so the
                        // wrong agent answers. Warn loudly per turn so it's
                        // visible in logs until config.toml is fixed.
                        warn!(
                            "default_agent '{name}' is not a loaded agent — \
                             routing fell back to the main agent; replies may \
                             come from the wrong agent. Fix `default_agent` in \
                             config.toml or remove it."
                        );
                        reg.main_agent()
                    }
                }
            } else {
                reg.main_agent()
            }
        }
    };

    if let Some(a) = agent {
        info!(
            "Using agent: {} ({})",
            a.config.agent.display_name, a.config.agent.name
        );
    }

    let model = agent
        .map(|a| a.config.model.preferred.clone())
        .unwrap_or_else(|| duduclaw_core::types::DEFAULT_PREFERRED_MODEL.to_string());

    let agent_id = agent
        .map(|a| a.config.agent.name.clone())
        .unwrap_or_default();

    // ── Goal intent router (P0, `goal_intent.rs`) ──────────────────────
    // Placed after the access gate and takeover interception above (never
    // bypasses either) and after `agent_id` resolution, so both the pending-
    // suggestion confirmation and the L0/L1 classifier have the same
    // resolved-agent identity every other gate on this path uses. A bare
    // "1"/"2"/"3" reply to a live suggestion is handled entirely here, at
    // zero LLM cost — the caller returns immediately without touching the
    // AI pipeline below. `goal_intent_precheck` (used further down, both for
    // the L2-B system-prompt injection and the post-reply `finalize` call)
    // is computed unconditionally so classification runs on every turn that
    // reaches this far, exactly once.
    if let Some(pending_reply) =
        crate::goal_intent::intercept_pending_confirmation(ctx, session_id, user_id, text).await
    {
        return pending_reply;
    }
    let goal_intent_precheck =
        crate::goal_intent::precheck(ctx, session_id, &agent_id, user_id, text).await;

    // OTel: record resolved agent/model on the `invoke_agent` span. The
    // channel-reply path is Claude-first (rotator/CLI/Direct API); a routed
    // non-Claude call carries its own provider on the nested `chat` span.
    {
        let span = tracing::Span::current();
        span.record(crate::otel::attrs::SYSTEM, "anthropic");
        span.record(crate::otel::attrs::PROVIDER_NAME, "anthropic");
        span.record(crate::otel::attrs::AGENT_NAME, agent_id.as_str());
        span.record(crate::otel::attrs::REQUEST_MODEL, model.as_str());
    }
    let agent_dir = agent.map(|a| a.dir.clone());
    let capabilities = agent.map(|a| a.config.capabilities.clone());

    // ── O-4: system-operator routing ────────────────────────────────────
    // ONLY for an agent explicitly opted in via `[capabilities]
    // system_operator = true` (the same capability O-0's MCP dispatch gate
    // requires) — every other agent skips this block entirely, so its reply
    // path stays byte-identical to before this existed (fail-open by
    // construction, not by exception handling). Placed after `agent_dir`/
    // `capabilities` resolve and after the goal-intent precheck above (never
    // races or overrides it — `os_operator::decide` returns `Continue` for
    // anything the goal-intent path should keep owning).
    //
    // `ShortCircuit` returns immediately: clarify/pending/rejected replies
    // are produced WITHOUT ever reaching the LLM, so a destructive intent
    // structurally cannot be auto-executed by this turn. `Guide` instead
    // carries a hint into the system prompt built further below (search
    // `operator_guide_hint`) — the model still has to make the actual tool
    // call, which still passes through every existing MCP gate unchanged.
    let mut operator_guide_hint: Option<String> = None;
    if capabilities
        .as_ref()
        .map(|c| c.system_operator)
        .unwrap_or(false)
    {
        let os_intent_result =
            crate::os_intent::route_os_intent(&ctx.home_dir, agent_dir.as_deref(), text).await;
        match crate::os_operator::decide(&os_intent_result) {
            crate::os_operator::OperatorAction::ShortCircuit(reply) => {
                crate::os_operator::audit_operator_decision(
                    &ctx.home_dir,
                    &agent_id,
                    text,
                    &os_intent_result,
                );
                return reply;
            }
            crate::os_operator::OperatorAction::Guide { hint, .. } => {
                crate::os_operator::audit_operator_decision(
                    &ctx.home_dir,
                    &agent_id,
                    text,
                    &os_intent_result,
                );
                operator_guide_hint = Some(hint);
            }
            crate::os_operator::OperatorAction::Continue => {}
        }
    }

    // G1: the agent's `[model] account_pool` narrows the rotator candidate set
    // (fail-open — see `AccountRotator::select_for_provider_with_pool`). Empty
    // when unset or when no agent resolved ⇒ rotation is unchanged.
    let account_pool: Vec<String> = agent
        .map(|a| a.config.model.account_pool.clone())
        .unwrap_or_default();
    let skill_token_budget = agent
        .map(|a| a.config.evolution.skill_token_budget)
        .unwrap_or(2500);
    let external_factors_config = agent
        .map(|a| a.config.evolution.external_factors.clone())
        .unwrap_or_default();

    // Cognitive memory layer. D7 (2026-08-04): this is no longer a toggle —
    // the layer is permanently resident, so every SqliteMemoryEngine path below
    // (key-fact recall into the system prompt, key-fact extraction/storage,
    // Reflexion → semantic-memory consolidation, conversation distillation) is
    // driven purely by whether a memory database path is configured.
    // `cognitive_memory_enabled()` is still consulted so a pre-D7 config that
    // says `false` logs its one-time deprecation warning instead of silently
    // changing behaviour.
    if let Some(a) = agent {
        let _ = a.config.evolution.cognitive_memory_enabled();
    }
    let cognitive_memory_db = ctx.memory_db_path.clone();

    // Refresh compressed skill cache from agent's loaded skills
    {
        let skills_data: Vec<(String, String, Option<String>)> = agent
            .map(|a| {
                a.skills
                    .iter()
                    .map(|s| (s.name.clone(), s.content.clone(), None))
                    .collect()
            })
            .unwrap_or_default();
        let mut cache = ctx.skill_cache.lock().await;
        cache.refresh(&skills_data);
    }

    // Get active skills for progressive injection
    let active_skills = {
        let ctrl = ctx.skill_activation.lock().await;
        ctrl.get_active(&agent_id)
    };

    // Build sub-agent team roster for system prompt injection.
    // Lists agents whose `reports_to` matches the current agent, so the agent
    // knows its team and can delegate via `spawn_agent` / `send_to_agent`.
    let team_members: Vec<TeamMember> = {
        let agents = reg.list();
        agents
            .iter()
            .filter(|a| a.config.agent.reports_to == agent_id && a.config.agent.name != agent_id)
            // F2: archived / soft-deleted sub-agents must not appear in the
            // "Your Team" roster — the agent should never be told to delegate
            // to an off-boarded teammate.
            .filter(|a| a.config.agent.status.is_operational())
            .map(|a| TeamMember {
                name: a.config.agent.name.clone(),
                display_name: a.config.agent.display_name.clone(),
                role: format!("{:?}", a.config.agent.role),
            })
            .collect()
    };
    let team_ref = if team_members.is_empty() {
        None
    } else {
        Some(team_members.as_slice())
    };

    // RFC-21 §1 step 4: resolve the sender's canonical identity *once* per
    // turn through the provider `config.toml [identity]` selects — wiki cache
    // (default), Notion, or the chained cache→upstream pair (G5 wired the
    // selection in; this used to be hard-coded to the wiki cache). The
    // formatted block is injected into the system prompt — agents no longer
    // need to grep `shared_wiki_read("identity/discord-users.md")`
    // mid-reasoning.
    let sender_block = build_sender_block(&ctx.home_dir, session_id, user_id).await;

    // P3-2 context-collapse defence: is this a 1:1 private chat (personal
    // context may be injected) or a group/shared session (Personal+ context
    // must be stripped)? Computed once per turn, fail-closed — anything not
    // provably 1:1 is treated as shared. Drives both the persona-block gate
    // below and the sensitivity-aware wiki injection inside build_system_prompt.
    let is_private = duduclaw_core::is_private_session(session_id, user_id);

    // WP: global default reply language (config.toml [general]
    // default_language), read once per turn — same cost/consistency
    // tradeoff as `get_default_agent` above. See `crate::prompt_identity`.
    let default_language = crate::prompt_identity::read_default_language(&ctx.home_dir).await;

    // Build progressive system prompt
    let system_prompt = {
        let cache = ctx.skill_cache.lock().await;
        let compressed: Vec<_> = cache.all().into_iter().cloned().collect();
        // turn_id keys this turn's citations (drain unit); session_id is the
        // budget unit for the per-conversation 0.10 cap. Both are needed —
        // see review BLOCKER R2-1 (cap was silently broken when conv_cap PK
        // followed turn_id and reset every turn).
        let citation_ctx = Some((agent_id.as_str(), turn_id.as_str(), Some(session_id)));
        if compressed.is_empty() {
            build_system_prompt(
                agent,
                None,
                None,
                None,
                skill_token_budget,
                team_ref,
                "",
                citation_ctx,
                &sender_block,
                is_private,
                default_language.as_deref(),
            )
        } else {
            build_system_prompt(
                agent,
                Some(text),
                Some(&compressed),
                Some(&active_skills),
                skill_token_budget,
                team_ref,
                "",
                citation_ctx,
                &sender_block,
                is_private,
                default_language.as_deref(),
            )
        }
    };
    drop(reg);

    let session_mgr = &ctx.session_manager;

    // ── L0: Safety word check (highest priority, zero latency) ──
    // Runs BEFORE session creation to avoid unnecessary DB writes for !STOP etc.
    let safety_action = duduclaw_security::safety_word::check(text, &ctx.killswitch.safety_words);
    if !matches!(
        safety_action,
        duduclaw_security::safety_word::SafetyWordAction::None
    ) {
        // Safety words are handled by chat_commands.rs, but if we reach here
        // (e.g., direct call without command parsing), handle inline
        match &safety_action {
            duduclaw_security::safety_word::SafetyWordAction::Stop(scope) => {
                if let Some(ref failsafe) = ctx.failsafe {
                    match scope {
                        duduclaw_security::safety_word::SafetyWordScope::CurrentScope => {
                            failsafe.force_halt(session_id, "safety word").await;
                            duduclaw_security::audit::log_safety_word(
                                &ctx.home_dir,
                                &agent_id,
                                session_id,
                                user_id,
                                "stop",
                            );
                            // C1 producer 甲 companion — see `security_autopilot.rs`.
                            crate::security_autopilot::emit_safety_word_triggered(&agent_id);
                            return duduclaw_security::safety_word::format_response(
                                &safety_action,
                                session_id,
                            );
                        }
                        duduclaw_security::safety_word::SafetyWordScope::Global => {
                            // Global stop requires admin — this inline path has no
                            // admin context, so only halt the current scope as a
                            // safeguard. The full !STOP ALL is handled via
                            // chat_commands::handle_command which enforces admin.
                            warn!(
                                session_id,
                                user_id,
                                "!STOP ALL via inline path — halting scope only (admin check unavailable)"
                            );
                            failsafe
                                .force_halt(session_id, "safety word: STOP ALL (scope-only)")
                                .await;
                            duduclaw_security::audit::log_safety_word(
                                &ctx.home_dir,
                                &agent_id,
                                session_id,
                                user_id,
                                "stop_all_downgraded",
                            );
                            // C1 producer 甲 companion — see `security_autopilot.rs`.
                            crate::security_autopilot::emit_safety_word_triggered(&agent_id);
                            return "🛑 Agent stopped (scope). Global stop requires admin — use chat command.".to_string();
                        }
                    }
                }
                return duduclaw_security::safety_word::format_response(&safety_action, session_id);
            }
            duduclaw_security::safety_word::SafetyWordAction::Resume => {
                if let Some(ref failsafe) = ctx.failsafe {
                    // Only resume the current scope — global halt requires
                    // explicit !STOP ALL scope to be cleared separately (via
                    // chat_commands handler which has user_id for admin check).
                    failsafe.resume(session_id).await;
                    duduclaw_security::audit::log_safety_word(
                        &ctx.home_dir,
                        &agent_id,
                        session_id,
                        user_id,
                        "resume",
                    );
                    // C1 producer 甲 companion — see `security_autopilot.rs`.
                    crate::security_autopilot::emit_safety_word_triggered(&agent_id);
                    return duduclaw_security::safety_word::format_response(
                        &safety_action,
                        session_id,
                    );
                }
                return "⚠️ Failsafe system not initialized.".to_string();
            }
            duduclaw_security::safety_word::SafetyWordAction::Status => {
                if let Some(ref failsafe) = ctx.failsafe {
                    let state = failsafe.get_state(session_id).await;
                    return duduclaw_security::failsafe::format_status(session_id, state.as_ref());
                }
                return "Failsafe: not initialized".to_string();
            }
            duduclaw_security::safety_word::SafetyWordAction::None => {}
        }
    }

    // ── L1: Failsafe state gate ──
    if let Some(ref failsafe) = ctx.failsafe {
        // Check global halt first
        let global_level = failsafe.get_level("__global__").await;
        let scope_level = failsafe.get_level(session_id).await;
        let effective_level = std::cmp::max(global_level, scope_level);

        use duduclaw_security::failsafe::FailsafeLevel;
        match effective_level {
            FailsafeLevel::L4Halted => {
                // Halted: reply with canned message
                return failsafe
                    .canned_reply(effective_level)
                    .unwrap_or("Service paused.")
                    .to_string();
            }
            FailsafeLevel::L3Muted => {
                // Muted: silent drop, no reply
                record_silent_reply(
                    &ctx.home_dir,
                    session_id,
                    user_id,
                    "silent_by_design: l3_muted",
                );
                return String::new();
            }
            FailsafeLevel::L2Restricted => {
                // Restricted: return canned reply, don't call AI
                return failsafe
                    .canned_reply(effective_level)
                    .unwrap_or("Service restricted.")
                    .to_string();
            }
            FailsafeLevel::L1Degraded => {
                // Degraded: allow through but could prefer local model
                // (model routing is handled downstream)
            }
            FailsafeLevel::L0Normal => {}
        }
    }

    // ── L2: Circuit breaker check ──
    let mut breaker_state = duduclaw_security::circuit_breaker::BreakerState::Closed;
    if let Some(ref cb_registry) = ctx.circuit_breakers {
        let decision = cb_registry.check_inbound(session_id, text).await;
        match decision {
            duduclaw_security::circuit_breaker::BreakerDecision::Allow => {}
            duduclaw_security::circuit_breaker::BreakerDecision::Throttle => {
                breaker_state = duduclaw_security::circuit_breaker::BreakerState::HalfOpen;
                // Allow through but mark for defensive prompt injection later
            }
            duduclaw_security::circuit_breaker::BreakerDecision::Deny(_) => {
                debug!(session_id, "Circuit breaker denied — message dropped");
                record_silent_reply(
                    &ctx.home_dir,
                    session_id,
                    user_id,
                    "silent_by_design: breaker_deny",
                );
                return String::new(); // silent drop
            }
            duduclaw_security::circuit_breaker::BreakerDecision::Trip(reason) => {
                warn!(session_id, reason = %reason, "Circuit breaker tripped");
                // Audit log
                duduclaw_security::audit::log_circuit_breaker_trip(
                    &ctx.home_dir,
                    &agent_id,
                    session_id,
                    &reason.to_string(),
                );
                // C1 producer 甲 companion — see `security_autopilot.rs`.
                crate::security_autopilot::emit_circuit_breaker_trip(&agent_id);
                record_silent_reply(
                    &ctx.home_dir,
                    session_id,
                    user_id,
                    &format!("silent_by_design: breaker_trip ({reason})"),
                );
                // Escalate failsafe
                if let Some(ref failsafe) = ctx.failsafe {
                    failsafe
                        .escalate(session_id, &format!("circuit breaker: {reason}"))
                        .await;
                }
                return String::new(); // silent drop for this message
            }
        }
    }

    // ── L2.5: Budget circuit breaker (cost enforcement) ──
    // If the agent has hit its hard spend cap, stop before any LLM call and tell
    // the user on their own channel — this reply IS the cross-channel budget
    // alert. Inert unless `agent.toml [budget]` sets a cap with `hard_stop`; the
    // check fails open if telemetry is unavailable.
    {
        let budget =
            crate::budget::check_agent_budget(&ctx.home_dir, agent_dir.as_deref(), &agent_id).await;
        if budget.is_denied() {
            return budget.user_message();
        }
    }

    // ── L3: Prompt injection scan (existing) ──
    // P0-2: use the audit-emitting variant so a blocked inbound injection
    // leaves a forensic trail in `security_audit.jsonl` (via
    // `log_injection_detected`) instead of being dropped silently.
    let scan = duduclaw_security::input_guard::scan_input_with_audit(
        text,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
        &ctx.home_dir,
        &agent_id,
    );
    if scan.blocked {
        warn!(
            agent = %agent_id,
            score = scan.risk_score,
            rules = ?scan.matched_rules,
            "Prompt injection detected — blocking message"
        );
        return format!("⚠️ {}", scan.summary);
    }

    // ── All pre-filters passed — now create/load session ──
    let _ = session_mgr.get_or_create(session_id, &agent_id).await;

    // ── Phase 3: Check if previous trajectory should get feedback ──
    // The current user message may contain feedback (positive/negative) for
    // the assistant's previous reply, completing the "within 2 turns" window.
    {
        let sentiment = detect_user_sentiment(text);
        if let Some(sentiment) = sentiment {
            let session_key = format!("{session_id}:{agent_id}");
            let mut recorder = ctx.skill_recorder.lock().await;
            if recorder.is_recording(&session_key) {
                // Record this feedback turn, then finalize with detected sentiment
                recorder.record_turn(&session_key, "user", text, vec![]);
                let outcome = match sentiment {
                    Sentiment::Positive => TrajectoryOutcome::Success,
                    Sentiment::Negative => TrajectoryOutcome::Failure,
                };
                if let Some(trajectory) = recorder.finalize(&session_key, outcome, Some(sentiment))
                {
                    // Extract skill heuristically (zero LLM cost)
                    if let Some(skill) = SkillExtractor::extract_heuristic(&trajectory) {
                        info!(
                            skill_name = %skill.name,
                            tools = ?skill.tools_used,
                            confidence = skill.confidence,
                            "Auto-extracted skill from trajectory (feedback-triggered)"
                        );

                        // Persist to SkillCache
                        {
                            let mut bank = ctx.skill_bank.lock().await;
                            bank.add(skill.clone());
                            debug!(bank_size = bank.len(), "Skill added to SkillCache");
                        }

                        // Log extraction event to audit log
                        let audit_entry = serde_json::json!({
                            "event": "skill_extracted",
                            "trigger": "user_feedback",
                            "skill_id": skill.id,
                            "skill_name": skill.name,
                            "tools_used": skill.tools_used,
                            "confidence": skill.confidence,
                            "sentiment": format!("{sentiment:?}"),
                            "source_session": session_key,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        });
                        if let Ok(audit_line) = serde_json::to_string(&audit_entry) {
                            let audit_path = ctx.home_dir.join("skill_extraction_audit.jsonl");
                            if let Ok(mut f) = tokio::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&audit_path)
                                .await
                            {
                                use tokio::io::AsyncWriteExt;
                                let _ = f.write_all(format!("{audit_line}\n").as_bytes()).await;
                            }
                        }
                    }
                }
                debug!(
                    session = %session_key,
                    sentiment = ?sentiment,
                    "User feedback detected for active trajectory"
                );
            }
        }
    }

    // Sanitize role-prefix injection: strip any attempt to impersonate assistant/system role
    let sanitized_text = if text.starts_with("assistant:") || text.starts_with("system:") {
        format!("[user input] {text}")
    } else {
        text.to_string()
    };

    // Prepend sender metadata so the agent can identify who is talking. This is
    // plumbing for the model, NOT something a human should ever read — strip it
    // with `strip_sender_prefix` on every display path (transcript replay,
    // conversation titles).
    let sanitized_text = if user_id != "anonymous" && !user_id.is_empty() {
        format!("{SENDER_PREFIX_OPEN}{user_id}]\n{sanitized_text}")
    } else {
        sanitized_text
    };

    // Append user message to session using improved CJK-aware token estimate
    let user_tokens = estimate_tokens(&sanitized_text);
    let user_message_id = match session_mgr
        .append_message_with_id(session_id, "user", &sanitized_text, user_tokens)
        .await
    {
        Ok(id) => Some(id),
        Err(e) => {
            warn!("Failed to save user message to session: {e}");
            None
        }
    };

    // Build structured conversation history from session (for native multi-turn).
    // Filter out "system" role messages — these are post-compression summaries
    // stored by SessionManager::compress(). They belong in the system prompt,
    // not in the conversation turns (Anthropic Messages API rejects them).
    //
    // #13 glue (2026-05-12): when the async summarizer task has folded
    // older turns into `summary_of_prior`, prepend the summary as a
    // synthetic `assistant` recap turn and skip the verbatim slice it
    // covers. Falls through to verbatim history when no summary exists
    // (summarizer hasn't run yet, or session is below the threshold).
    let max_history_turns = 20;
    let mut compression_summary = String::new();
    let (async_summary, summarized_through) = session_mgr
        .get_summary(session_id)
        .await
        .unwrap_or_default();
    let conversation_history: Vec<ConversationTurn> =
        match session_mgr.get_messages(session_id).await {
            Ok(msgs) => {
                // Optional prefix when the summarizer task has run for this
                // session. Encoded as a single assistant-role turn so the
                // Messages API doesn't reject it (no `system` role in turns).
                let mut out: Vec<ConversationTurn> = Vec::new();
                if !async_summary.trim().is_empty() {
                    out.push(ConversationTurn {
                        role: "assistant".to_string(),
                        content: format!(
                            "[summary of earlier turns 1..={summarized_through}]\n{async_summary}"
                        ),
                    });
                }

                // Verbatim slice: skip the first `summarized_through` turns
                // (already captured in the prefix) and the LAST turn (which
                // is the user message about to be re-sent below). The +1
                // index skip is intentional — we want messages.len() - 1
                // minus the summarized prefix.
                let summarized_through_usize = summarized_through as usize;
                let prior_full: Vec<_> = msgs
                    .iter()
                    .take(msgs.len().saturating_sub(1))
                    .filter_map(|m| {
                        if m.role == "system" {
                            // Capture compression summary for system prompt injection
                            if !m.content.is_empty() {
                                compression_summary = m.content.clone();
                            }
                            None
                        } else {
                            Some(ConversationTurn {
                                role: m.role.clone(),
                                content: m.content.clone(),
                            })
                        }
                    })
                    .collect();
                // Trim already-summarized turns (best-effort: the count we
                // skip is approximate because the summarizer indexes raw
                // messages, including potential hidden ones — but trimming
                // a bit conservatively is fine, the model sees the summary
                // either way).
                let prior: Vec<_> = if summarized_through_usize > 0
                    && prior_full.len() > summarized_through_usize
                {
                    prior_full[summarized_through_usize..].to_vec()
                } else if summarized_through_usize >= prior_full.len() {
                    Vec::new()
                } else {
                    prior_full
                };

                // Keep only the most recent turns to prevent token overflow
                let trimmed = if prior.len() > max_history_turns {
                    prior[prior.len() - max_history_turns..].to_vec()
                } else {
                    prior
                };
                out.extend(trimmed);
                out
            }
            Err(e) => {
                warn!("Failed to load session messages: {e}");
                vec![]
            }
        };
    let has_history = !conversation_history.is_empty();

    // ── Instruction Pinning: load + accumulate ──
    // Pinned instructions survive session compression (stored on sessions table).
    let mut pinned = session_mgr.get_pinned(session_id).await.unwrap_or_default();

    // Clarification accumulation: if agent asked a question last turn and user
    // is now answering, append the answer to pinned instructions.
    if has_history && !pinned.is_empty() {
        if let Some(last_assistant) = conversation_history
            .iter()
            .rev()
            .find(|t| t.role == "assistant")
        {
            if last_assistant.content.contains('？') || last_assistant.content.contains('?') {
                let answer_snippet = duduclaw_core::truncate_bytes(&sanitized_text, 200);
                // Cap pinned at ~1000 chars to prevent bloat
                if pinned.len() < 1000 {
                    pinned = format!("{pinned}\n- 用戶確認：{answer_snippet}");
                    let _ = session_mgr.set_pinned(session_id, &pinned).await;
                }
            }
        }
    }

    // Inject key facts + pinned instructions + compression summary into system prompt.
    // Order: key facts (middle) → compression summary → pinned (tail, highest attention).
    // Consolidated-rule ids injected this turn (ACE/ExpeL lifecycle) — settled
    // against the prediction outcome in the spawned task below.
    let mut injected_rule_ids: Vec<String> = Vec::new();
    // v1.54 dialogue shadow-scoring: read the held-out gate once per turn so
    // the build-time arming below and the settle-time scoring in the spawned
    // prediction task always agree on the same flag value. `armed_shadow`
    // captures the shadow candidates whose signals matched this turn — never
    // injected, graded out-of-sample at settle.
    let held_out_gate_enabled =
        crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(&ctx.home_dir)
            .held_out_gate_enabled;
    let mut armed_shadow = crate::playbook::ArmedShadow::default();
    let full_system_prompt = {
        let mut prompt = system_prompt;

        // P2 Key-Fact Accumulator: inject cross-session facts (middle position —
        // stable reference data that doesn't need U-shaped peak attention).
        // Uses spawn_blocking because SqliteMemoryEngine is !Send (rusqlite).
        //
        // P3-2 context-collapse: these are facts *about this user* (persona,
        // Personal sensitivity). In a group/shared session they must not be
        // stitched into a prompt other members see — withhold entirely.
        if !is_private && cognitive_memory_db.is_some() {
            tracing::debug!(
                session_id,
                "P3-2 context-collapse: withholding persona 'Key Facts About This User' from a shared session"
            );
        }
        if let Some(db_path) = cognitive_memory_db.clone().filter(|_| is_private) {
            let aid = agent_id.clone();
            let query = sanitized_text.clone();
            let home_for_facts = ctx.home_dir.clone();
            if let Ok(facts) = tokio::task::spawn_blocking(move || {
                // H4: single construction point (`[memory] novelty_gate` + `w_vec`).
                let engine =
                    crate::memory_factory::build_memory_engine(&db_path, &home_for_facts).ok()?;
                let rt = tokio::runtime::Handle::current();
                let facts = rt.block_on(engine.search_facts(&aid, &query, 3)).ok()?;
                if facts.is_empty() {
                    return None;
                }
                Some(
                    facts
                        .iter()
                        .map(|f| f.fact.clone())
                        .collect::<Vec<String>>(),
                )
            })
            .await
            {
                if let Some(facts) = facts {
                    // Wiki/memory dedup: the base prompt already carries the
                    // injected wiki pages — a fact whose text is already in
                    // there would be sent twice. Wiki wins (curated, trust-
                    // scored, citation-tracked); the duplicate fact is dropped.
                    let kept = filter_facts_not_in_prompt(&facts, &prompt);
                    if !kept.is_empty() {
                        let ft = kept
                            .iter()
                            .map(|f| format!("- {f}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        prompt = format!("{prompt}\n\n## Key Facts About This User\n{ft}");
                    }
                }
            }
        }

        // WP1.3: turn-signal assembly for playbook injection below.
        // `channel:` from the session id's leading segment (established
        // convention — see the identical split a few lines above in agent
        // resolution); `kw:` from the message; `mistake:`/`source_kind:`
        // folded in below once the mistake query (already needed for F2a)
        // runs, so the same query result is reused rather than re-fetched.
        let channel_name = session_id.split(':').next().unwrap_or("unknown");
        let mut turn_signals = crate::playbook::TurnSignals::new()
            .with_channel(channel_name)
            .with_keywords_from_message(&sanitized_text);

        // F2a (Reflexion recall): surface this agent's recent unresolved mistakes
        // into the answering prompt — not just the GVU Generator (SOUL.md path).
        // Bridges MistakeNotebook → cross-task learning so the agent avoids
        // repeating past failures on similar topics.
        if let Some(ref nb) = ctx.mistake_notebook {
            // Topic-scoped recall first (whitespace keywords); fall back to most
            // recent unresolved so CJK queries (no whitespace tokens) aren't empty.
            let kw: Vec<&str> = sanitized_text
                .split_whitespace()
                .filter(|w| w.chars().count() >= 3)
                .take(12)
                .collect();
            let mut mistakes = if kw.is_empty() {
                nb.query_by_agent(&agent_id, 3)
            } else {
                nb.query_by_topic(&kw, &agent_id, 3)
            };
            if mistakes.is_empty() {
                mistakes = nb.query_by_agent(&agent_id, 3);
            }
            for m in &mistakes {
                turn_signals = turn_signals
                    .with_mistake_category(m.category.as_str())
                    .with_source_kind(&m.source_kind);
            }
            if !mistakes.is_empty() {
                let section = mistakes
                    .iter()
                    .map(|m| m.to_prompt_section())
                    .collect::<Vec<_>>()
                    .join("\n");
                prompt = format!("{prompt}\n\n## Past Mistakes to Avoid\n{section}");
            }
        }

        // F2a extension (ACE/ExpeL rule lifecycle) → WP1.3 playbook
        // injection: signal-matched entries (built from `turn_signals` above)
        // rank ahead of score-only fill, under an explicit byte budget
        // (`InjectionBudget`) so the section can never grow unbounded — see
        // `playbook::select` module doc / DESIGN-evolution-v3-aee.md §1.8.
        // Retired/Stale entries are filtered out inside the selector; the
        // injected ids are settled against this turn's prediction outcome
        // below via the SAME unmodified `rule_lifecycle::settle_injected_rules`
        // (it operates by id, agnostic of which source_event produced the
        // row). !Send → spawn_blocking.
        if let Some(db_path) = cognitive_memory_db.clone() {
            let aid = agent_id.clone();
            let signals_for_task = turn_signals.clone();
            let max_input_tokens = agent_dir
                .as_deref()
                .and_then(crate::prompt_audit::read_max_input_tokens);
            let budget = crate::playbook::InjectionBudget::from_max_input_tokens(max_input_tokens);
            // v1.54 shadow-scoring closure: when the held-out gate is on, the
            // same blocking hop also collects the shadow candidates whose
            // signals match this turn ("armed") — they are never injected,
            // but the settle task below grades each of them out-of-sample
            // against this turn's final error category. Gate off ⇒ the scan
            // is skipped and this block is byte-identical to before.
            let arm_shadow = held_out_gate_enabled;
            if let Ok((section, armed)) = tokio::task::spawn_blocking(move || {
                let section = crate::playbook::build_playbook_section_blocking(
                    &db_path,
                    &aid,
                    &signals_for_task,
                    budget,
                );
                let armed = if arm_shadow {
                    crate::playbook::collect_armed_shadow_blocking(
                        &db_path,
                        &aid,
                        &signals_for_task,
                    )
                } else {
                    crate::playbook::ArmedShadow::default()
                };
                (section, armed)
            })
            .await
            {
                if let Some((section, ids)) = section {
                    prompt = format!("{prompt}\n\n{section}");
                    injected_rule_ids = ids;
                }
                armed_shadow = armed;
            }
        }

        // B3 cross-session user profile: inject a session-stable
        // `## About This User` block of the sender's accumulated preference
        // traits (subject = `user:<user_id>`). Keyed by (agent_id, user_id);
        // deterministic bytes → prompt-cache friendly. Empty profile ⇒ no-op.
        // !Send → spawn_blocking.
        //
        // P3-2 context-collapse: this is a Personal-sensitivity persona block —
        // withheld from group/shared sessions (only the 1:1 sender should see
        // their own accumulated profile).
        if !is_private && cognitive_memory_db.is_some() {
            tracing::debug!(
                session_id,
                "P3-2 context-collapse: withholding persona '## About This User' from a shared session"
            );
        }
        if let Some(db_path) = cognitive_memory_db.clone().filter(|_| is_private) {
            let aid = agent_id.clone();
            let uid = user_id.to_string();
            let home_for_profile = ctx.home_dir.clone();
            if let Ok(Some(section)) = tokio::task::spawn_blocking(move || {
                // H4: single construction point (`[memory] novelty_gate` + `w_vec`).
                let engine =
                    crate::memory_factory::build_memory_engine(&db_path, &home_for_profile).ok()?;
                let rt = tokio::runtime::Handle::current();
                rt.block_on(duduclaw_memory::user_profile::profile_block(
                    &engine, &aid, &uid,
                ))
                .ok()
                .flatten()
            })
            .await
            {
                // `section` already begins with the `## About This User` header.
                prompt = format!("{prompt}\n\n{section}");
            }
        }

        // RFC-24 (F1 injection): surface this agent's still-open decisions so a
        // later "用方案 C" resolves from durable state, not conversation memory.
        // Tail placement (near pinned, U-shaped peak attention). Own opt-in
        // flag (`[memory] decision_continuity`). !Send → spawn_blocking.
        if agent_dir
            .as_deref()
            .map(crate::runtime_config::decision_continuity_enabled)
            .unwrap_or(false)
        {
            if let Some(db_path) = ctx.memory_db_path.clone() {
                let aid = agent_id.clone();
                let home_for_decisions = ctx.home_dir.clone();
                if let Ok(section) = tokio::task::spawn_blocking(move || {
                    // H4: single construction point (`[memory] novelty_gate` + `w_vec`).
                    let engine =
                        crate::memory_factory::build_memory_engine(&db_path, &home_for_decisions)
                            .ok()?;
                    let rt = tokio::runtime::Handle::current();
                    let s = rt.block_on(crate::decision_capture::build_open_decisions_section(
                        &engine, &aid,
                    ));
                    if s.is_empty() { None } else { Some(s) }
                })
                .await
                {
                    if let Some(s) = section {
                        prompt = format!("{prompt}\n\n{s}");
                    }
                }
            }
        }

        // WP-6F (agent presets P1): the agent-visible preset line — placed
        // BEFORE working_state (design §3.2: "preset 行接在它前面即可"). Tail
        // placement, after CACHE_SPLIT_MARKER — a preset switch must be
        // visible to the agent, never silently baked into the cached prefix.
        {
            let home = ctx.home_dir.clone();
            let aid = agent_id.clone();
            if let Ok(Some(section)) = tokio::task::spawn_blocking(move || {
                crate::preset_prompt::build_preset_section(&home, &aid)
            })
            .await
            {
                prompt = format!("{prompt}\n\n{section}");
            }
        }

        // Cross-wake working state: the agent's authoritative key-value
        // posture + handoff note (working_state.rs, D3 ghost-memory fix).
        // Placed BEFORE the recent-actions feed — standing authority first,
        // action evidence second. Tail placement, after CACHE_SPLIT_MARKER.
        {
            let home = ctx.home_dir.clone();
            let aid = agent_id.clone();
            if let Ok(Some(section)) = tokio::task::spawn_blocking(move || {
                crate::working_state::build_working_state_section(&home, &aid)
            })
            .await
            {
                prompt = format!("{prompt}\n\n{section}");
            }
        }

        // Cross-invocation continuity: recent self-action feed from the
        // audit log — the channel run opens aware of what this agent already
        // did in scheduled/heartbeat/goal-loop invocations, so it can't deny
        // its own recorded actions (blocked/failed ones included). Tail
        // placement, after CACHE_SPLIT_MARKER — never in the cached prefix.
        {
            let home = ctx.home_dir.clone();
            let aid = agent_id.clone();
            if let Ok(Some(section)) = tokio::task::spawn_blocking(move || {
                crate::recent_actions::build_recent_actions_section(&home, &aid)
            })
            .await
            {
                prompt = format!("{prompt}\n\n{section}");
            }
        }

        // Goal intent router (P0) — L2-B grey-band instruction. Only present
        // when THIS turn's own L0/L1 score landed in the grey band
        // (`goal_intent_precheck`, computed once above); a per-turn
        // condition, so it must never enter the cached prefix. Fixed text,
        // no user input embedded — see `goal_intent::l2b_reply_tag_instruction`.
        if matches!(
            goal_intent_precheck.action,
            crate::goal_intent::GoalIntentAction::GrayCandidate
        ) {
            prompt = format!(
                "{prompt}\n\n{}",
                crate::goal_intent::l2b_reply_tag_instruction()
            );
        }

        // O-4: system-operator guidance. Only present when `os_operator::decide`
        // (computed once above, before this prompt-build block) resolved this
        // turn to a ready, non-destructive `SystemOp` for a `system_operator`-
        // capable agent — a per-turn condition, so it must never enter the
        // cached prefix, same placement rule as the goal-intent block above.
        if let Some(hint) = &operator_guide_hint {
            prompt = format!("{prompt}\n\n{hint}");
        }

        if !compression_summary.is_empty() {
            prompt = format!("{prompt}\n\n## Prior Conversation Summary\n{compression_summary}");
        }
        if !pinned.is_empty() {
            prompt = format!(
                "{prompt}\n\n## Pinned Task Instructions\n\
                 The user's core task requirements (ALWAYS follow these throughout the conversation):\n\
                 {pinned}"
            );
        }
        prompt
    };
    let full_system_prompt = match crate::ccr_runtime::historical_reference_note(
        &session_mgr,
        &ctx.home_dir,
        &agent_id,
        session_id,
        text,
    )
    .await
    {
        Some(note) => format!("{full_system_prompt}\n\n{note}"),
        None => full_system_prompt,
    };

    // Track the last underlying failure so the fallback message can
    // accurately describe what went wrong (rate limit vs timeout vs
    // missing binary etc.) instead of always blaming "not installed".
    let mut last_cli_error: Option<String> = None;

    // Record the moment we dispatched the CLI call. This is the lower
    // time bound used by the action-claim verifier when scanning
    // tool_calls.jsonl for receipts that back up the agent's text
    // assertions — anything before this timestamp belongs to a
    // previous turn and must not be credited to this one.
    let dispatch_start_time = chrono::Utc::now().to_rfc3339();

    // ── L5 Computer Use: intercept if agent has computer_use enabled ──
    // Check for natural-language emergency stop first
    if crate::risk_detector::is_emergency_stop(text) {
        info!(session_id, "Emergency stop detected for computer use");
        // Stop ALL active computer use sessions via the global registry
        let sessions = crate::computer_use_orchestrator::list_sessions().await;
        for sid in &sessions {
            if let Some(ctl) = crate::computer_use_orchestrator::get_session_control(sid).await {
                ctl.stopped
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            crate::computer_use_orchestrator::unregister_session(sid).await;
        }
        let count = sessions.len();
        return if count > 0 {
            format!("🛑 已停止 {count} 個電腦操作 session")
        } else {
            "🛑 已停止電腦操作".to_string()
        };
    }

    // Check if this agent has computer_use enabled and the user's intent
    // suggests a computer use task (e.g., mentions screen, click, open app).
    let cu_enabled = capabilities
        .as_ref()
        .map(|c| c.computer_use)
        .unwrap_or(false);

    if cu_enabled && looks_like_computer_use_request(text) {
        // Build a ComputerUseConfig from the agent's capabilities
        let cap_cfg = capabilities
            .as_ref()
            .map(|c| &c.computer_use_config)
            .cloned()
            .unwrap_or_default();
        // Read execution_mode from capabilities
        let exec_mode = capabilities
            .as_ref()
            .map(|c| c.computer_use_mode)
            .unwrap_or_default();

        // Read CONTRACT.toml must_not rules (if the agent has a contract)
        let contract_must_not = agent_dir
            .as_ref()
            .and_then(|d| {
                let contract_path = d.join("CONTRACT.toml");
                let content = std::fs::read_to_string(&contract_path).ok()?;
                let table: toml::Table = content.parse().ok()?;
                let must_not = table.get("must_not")?.as_table()?;
                let rules = must_not.get("rules")?.as_array()?;
                Some(
                    rules
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default();

        let cu_config = crate::computer_use_orchestrator::ComputerUseConfig {
            max_session_minutes: cap_cfg.max_session_minutes,
            max_actions: cap_cfg.max_actions,
            display_width: cap_cfg.display_width,
            display_height: cap_cfg.display_height,
            auto_confirm_trusted: cap_cfg.auto_confirm_trusted,
            allowed_apps: cap_cfg.allowed_apps.clone(),
            blocked_actions: cap_cfg.blocked_actions.clone(),
            execution_mode: exec_mode,
            contract_must_not,
            ..Default::default()
        };

        // Resolve API key for the Claude Vision API (computer use needs direct API)
        if let Some(api_key) = get_api_key(&ctx.home_dir).await {
            let mut orchestrator = crate::computer_use_orchestrator::ComputerUseOrchestrator::new(
                agent_id.clone(),
                ctx.home_dir.clone(),
                cu_config,
            );

            // Build a real channel sender from the session_id (e.g., "telegram:12345")
            // so screenshots and confirmations are delivered to the user's channel.
            let sender: Box<dyn crate::channel_sender::ChannelSender> = {
                let (ch_type, ch_id) = parse_session_id_parts(session_id);
                if ch_type.is_empty() || ch_id.is_empty() {
                    Box::new(crate::channel_sender::NullSender)
                } else if ch_type == "webchat" {
                    // WebChat needs the broadcast tx for WebSocket delivery
                    crate::channel_sender::create_webchat_sender(
                        ch_id.to_string(),
                        ctx.event_tx.clone(),
                    )
                } else if ch_type == "googlechat" {
                    // Space name may contain '/', which the generic split handles;
                    // credentials come from config via home_dir.
                    crate::channel_sender::create_googlechat_sender(
                        ctx.home_dir.clone(),
                        session_id
                            .strip_prefix("googlechat:")
                            .unwrap_or(ch_id)
                            .to_string(),
                        user_id.to_string(),
                    )
                } else if let Some(conv_id) = session_id.strip_prefix("teams:") {
                    // Teams conversation ids contain ':' — take the full
                    // remainder, not the colon-split second segment.
                    crate::channel_sender::create_teams_sender(
                        ctx.home_dir.clone(),
                        conv_id.to_string(),
                        user_id.to_string(),
                    )
                } else {
                    // Look up the channel token from config
                    let token = crate::config_crypto::read_encrypted_config_field(
                        &ctx.home_dir,
                        ch_type,
                        &format!("{ch_type}_bot_token"),
                    )
                    .await
                    .unwrap_or_default();

                    let target = crate::channel_sender::ChannelTarget {
                        channel_type: ch_type.to_string(),
                        chat_id: ch_id.to_string(),
                        token,
                        extra_id: Some(user_id.to_string()),
                    };
                    crate::channel_sender::create_sender(&target, ctx.http.clone())
                }
            };

            // Generate a session ID and register in the global registry
            let cu_session_id = format!("cu-{}", uuid::Uuid::new_v4().as_simple());
            let control = orchestrator.control_handle();

            match orchestrator.start_session(&api_key, &model).await {
                Ok(()) => {
                    // Register session so /stop, emergency stop, and MCP tools can find it
                    if let Err(e) =
                        crate::computer_use_orchestrator::register_session(&cu_session_id, control)
                            .await
                    {
                        warn!(error = %e, "Failed to register computer use session");
                        orchestrator.stop_session().await;
                        // Fall through to text reply
                    } else {
                        let result = orchestrator.run_loop(text, sender.as_ref()).await;

                        // Always unregister on completion
                        crate::computer_use_orchestrator::unregister_session(&cu_session_id).await;

                        match result {
                            Ok(reply_text) => return reply_text,
                            Err(e) => {
                                warn!(error = %e, "Computer use session failed, falling back to text");
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to start computer use container, falling back to text");
                }
            }
        }
    }

    // 1. Try `claude` CLI with multi-account rotation (OAuth + API keys)
    // Wrap in REPLY_CHANNEL scope so `send_to_agent` MCP tool can register
    // delegation callbacks for sub-agent response forwarding.
    // Only set for sessions originating from a real channel (telegram/line/discord).
    // Snowball Recap: prepend pinned instructions as <task_recap> to user message.
    // Placed in user message (U-shaped attention tail peak) rather than system
    // prompt to maximize LLM attention on the original task requirements.
    let effective_message = if pinned.is_empty() || !has_history {
        sanitized_text.clone()
    } else {
        format!("<task_recap>\n{pinned}\n</task_recap>\n\n{sanitized_text}")
    };

    // #12 glue (2026-05-12) — request-boundary budget enforcement.
    //
    // Read the agent's `[budget] max_input_tokens` (0 = disabled,
    // back-compat). If the total estimated prompt is over budget, run
    // the compression pipeline on `conversation_history`. The
    // `cost_pressure` flag from #6.3 makes early stages more aggressive.
    //
    // Failure mode is intentionally NON-fatal: if the pipeline can't
    // bring us under budget, we log a warn + emit an evolution event
    // and proceed with the full history. Rejecting the request would
    // surprise the user with a silent failure mid-conversation; the
    // 200 K cliff merely doubles input price, it doesn't break the
    // call. Future work can flip this to hard-reject behind a flag.
    let (conversation_history, compression_info) = maybe_compress_history(
        &full_system_prompt,
        conversation_history,
        &effective_message,
        &agent_id,
    )
    .await;

    // RFC-25 Phase 1 (L8): provider-agnostic routing via the centralized
    // decision predicate. When the agent's `[runtime] provider` is not Claude,
    // route the whole reply through the multi-runtime choke-point (Codex /
    // Gemini / OpenAI-compat). Claude keeps its optimized OAuth-rotation + PTY
    // path below (unchanged, zero regression).
    // Parse agent.toml once (L7 followup): the routing decision and the
    // choke-point both need it, so load here and thread the settings through
    // `AgentPrompt.runtime_settings` instead of re-reading inside the choke-point.
    let runtime_settings = agent_dir
        .as_deref()
        .map(crate::runtime_config::load_runtime_settings);
    let non_claude = runtime_settings
        .as_ref()
        .and_then(|s| s.non_claude_provider());

    let cli_future: std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>,
    > = if duduclaw_llm::is_moa_model_id(&model) {
        // MoA virtual model (`moa:<name>`) — API-mode only. A CLI spawn can
        // never serve an ensemble (and `claude -p --model moa:x` would just
        // 404 upstream), so route straight through the duduclaw-llm MoA
        // executor. The CLI-spawn helpers below also hard-reject `moa:` ids
        // defensively.
        info!(agent_id = %agent_id, model = %model, "channel_reply: routing through MoA ensemble (API mode)");
        let moa_model = model.as_str();
        let moa_system = full_system_prompt.as_str();
        let moa_prompt = effective_message.as_str();
        let moa_home = ctx.home_dir.as_path();
        let moa_agent = agent_id.as_str();
        // HIGH-B: thread the real session history + attribute cost telemetry
        // to the calling agent (same inputs the non-MoA direct path gets).
        let moa_history: Vec<(String, String)> = conversation_history
            .iter()
            .map(|t| (t.role.clone(), t.content.clone()))
            .collect();
        Box::pin(async move {
            crate::direct_api::call_moa_model(
                moa_home,
                moa_agent,
                crate::cost_telemetry::RequestType::Chat,
                moa_model,
                moa_system,
                moa_prompt,
                &moa_history,
            )
            .await
            .map_err(|e| format!("MoA 模型 `{moa_model}` 需要 API 模式（無法經由 CLI 執行）：{e}"))
        })
    } else if let Some(provider) = non_claude {
        info!(
            agent_id = %agent_id,
            provider = provider.as_str(),
            "channel_reply: routing through multi-runtime choke-point (non-Claude provider)"
        );
        // RFC-25 A4: non-Claude runtimes don't stream incremental progress, so
        // emit a periodic Keepalive while the (potentially long) call is in flight
        // — same typing/"still working" indicator the Claude stream-json path
        // drives — so the channel doesn't look stalled or hit an idle timeout.
        // Bind plain references first so the `async move` captures only Copy
        // references (not the owners, which the Claude `else` arm still borrows).
        let hb_progress = on_progress.as_ref();
        let hb_agent_dir = agent_dir.as_deref();
        let hb_home = ctx.home_dir.as_path();
        let hb_agent_id = agent_id.as_str();
        let hb_prompt = effective_message.as_str();
        let hb_system = full_system_prompt.as_str();
        let hb_model = model.as_str();
        let hb_history = conversation_history.as_slice();
        let hb_settings = runtime_settings.as_ref();
        // Observability fix (2026-07-23 distributor incident): a silent
        // failover — the configured provider's CLI missing/unavailable so
        // `execute_with_failover` fell through to Claude — used to be
        // invisible to the end user (they think they're talking to Grok while
        // Claude actually answered). `run_agent_prompt` (not the `_text`
        // convenience wrapper) is used here so `RuntimeResponse::runtime_name`
        // / `model_used` survive to compare against what was requested.
        let hb_session_id = session_id;
        Box::pin(async move {
            let work =
                crate::runtime_dispatch::run_agent_prompt(crate::runtime_dispatch::AgentPrompt {
                    agent_dir: hb_agent_dir,
                    home_dir: hb_home,
                    agent_id: hb_agent_id,
                    prompt: hb_prompt,
                    system_prompt: hb_system,
                    model: hb_model,
                    max_tokens: 8192,
                    provider_override: None,
                    // RFC-25 A1: thread the real session history so non-Claude
                    // (Codex/Gemini/OpenAI) agents keep multi-turn context.
                    conversation_history: hb_history,
                    request_type: crate::cost_telemetry::RequestType::Chat,
                    // L7 followup: reuse the settings parsed above (1 read/reply).
                    runtime_settings: hb_settings,
                    effort: None,
                    // P0/WP-B: ordinary caller — failover behavior unchanged.
                    allow_cross_family_failover: true,
                });
            tokio::pin!(work);
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(KEEPALIVE_INTERVAL_SECS));
            ticker.tick().await; // consume the immediate first tick
            let resp = loop {
                tokio::select! {
                    res = &mut work => break res,
                    _ = ticker.tick() => {
                        if let Some(cb) = hb_progress {
                            cb(ProgressEvent::Keepalive);
                        }
                    }
                }
            }?;
            // Detect and surface a runtime substitution — the response was
            // actually produced by a different backend than `[runtime]
            // provider` configured (failover happened inside the choke-point,
            // e.g. primary CLI not registered). Byte-identical behavior when
            // no substitution occurred.
            if is_runtime_substitution(provider.as_str(), &resp.runtime_name) {
                warn!(
                    agent_id = %hb_agent_id,
                    requested = provider.as_str(),
                    actual = %resp.runtime_name,
                    "channel_reply: non-Claude runtime substituted by failover — user is receiving \
                     a reply from a different backend than the agent's configured provider"
                );
                let record = serde_json::json!({
                    "event": "runtime_fallback_substitution",
                    "agent": hb_agent_id,
                    "session_id": hb_session_id,
                    // W2-4: platform attribution; `null` off-channel.
                    "channel": crate::trajectory_guard::channel_from_session_id(hb_session_id),
                    "requested": provider.as_str(),
                    "actual": resp.runtime_name,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                if let Err(e) = crate::trajectory_guard::append_anomaly(hb_home, &record) {
                    warn!(error = %e, "runtime_fallback_substitution: 寫入 channel_failures.jsonl 失敗");
                }
                // Dashboard-only signal (WebChat), same channel as the
                // stream-json ModelInfo events — tells the user which model
                // actually answered instead of silently substituting.
                if let Some(cb) = hb_progress {
                    cb(ProgressEvent::ModelInfo {
                        model: format!("{}（備援）", resp.model_used),
                    });
                }
            }
            Ok(resp.content)
        })
    } else {
        Box::pin(call_claude_cli_rotated(
            &effective_message,
            &model,
            &full_system_prompt,
            &ctx.home_dir,
            agent_dir.as_deref(),
            on_progress.as_ref(),
            capabilities.as_ref(),
            if has_history { Some(session_id) } else { None },
            &conversation_history,
            &account_pool,
            None,
        ))
    };
    let is_channel_session = duduclaw_core::SUPPORTED_CHANNEL_TYPES
        .iter()
        .any(|t| session_id.starts_with(&format!("{t}:")));

    // ── 0. inference_mode = "local" → local inference FIRST ─────────
    //
    // Long-standing gap: `[general] inference_mode` (config.toml) was only
    // honored by the dispatcher path; the user-facing channel reply always
    // went CLI-first. When the operator pins mode "local", prefer local
    // inference FIRST here too, keeping the Claude CLI as the fallback.
    // Absent / "hybrid" / "claude" ⇒ behavior unchanged (config-gated).
    //
    // The agent's `[model] local.model` (agent.toml) is resolved once here
    // and shared with the step-2 fallback below.
    let local_model_id = agent_dir.as_ref().and_then(|d| {
        let toml_path = d.join("agent.toml");
        let content = std::fs::read_to_string(&toml_path).ok()?;
        let table: toml::Table = content.parse().ok()?;
        table
            .get("model")?
            .as_table()?
            .get("local")?
            .as_table()?
            .get("model")?
            .as_str()
            .map(|s| s.to_string())
    });
    let inference_mode = crate::claude_runner::get_inference_mode(&ctx.home_dir).await;
    let mut local_attempted_first = false;
    let mut local_first_reply: Option<String> = None;
    if local_inference_first(&inference_mode) {
        local_attempted_first = true;
        match crate::claude_runner::try_local_inference(
            &ctx.home_dir,
            &sanitized_text,
            &full_system_prompt,
            local_model_id.as_deref(),
            Some(&agent_id),
            capabilities.as_ref(),
        )
        .await
        {
            Ok(local_reply) => {
                info!(
                    "Replied via local model ({} chars, inference_mode=local — CLI skipped)",
                    local_reply.len()
                );
                local_first_reply = Some(local_reply);
            }
            Err(e) if e == "ROUTER_ESCALATE_TO_CLOUD" => {
                info!(
                    "inference_mode=local: router escalated to cloud → falling back to Claude CLI"
                );
            }
            Err(e) => {
                warn!(
                    "inference_mode=local but local inference failed → falling back to Claude CLI: {e}"
                );
            }
        }
    }
    // (review B2) Make `turn_id` and `session_id` available to sub-agent
    // dispatchers via tokio task-locals. Any wiki RAG triggered by the
    // dispatcher inherits these so citations land in the right tracker
    // bucket AND respect the session-scoped per-conv cap.
    let cli_future = duduclaw_memory::feedback::CURRENT_SESSION_ID
        .scope(Some(session_id.to_string()), cli_future);
    let cli_future =
        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn_id.clone()), cli_future);
    // RFC-22 P1-7: scope CHANNEL_REPLY_AGENT_ID so spawn_claude_cli_with_env
    // can record cost_telemetry against the correct agent. agent_id is empty
    // when no agent resolved — scope an empty string in that case; the spawn
    // path checks for non-empty before calling cost_telemetry.
    let cli_future =
        crate::claude_runner::CHANNEL_REPLY_AGENT_ID.scope(agent_id.clone(), cli_future);
    // WP6: scope the end-user id so the token-usage recorder can attribute
    // spend per employee. Empty user_id ⇒ recorded as unattributed.
    let cli_future =
        crate::claude_runner::CHANNEL_REPLY_USER_ID.scope(user_id.to_string(), cli_future);
    // WP5: scope the compression outcome computed above so the eventual
    // `cost_telemetry` record call (several async frames away, inside
    // `spawn_claude_cli_with_env` / the PTY variant) can persist whether
    // this request's history was compressed and by which stages.
    let cli_future = crate::prompt_compression::CHANNEL_REPLY_COMPRESSION
        .scope(compression_info.clone(), cli_future);
    let local_first_answered = local_first_reply.is_some();
    let reply = match local_first_reply {
        // Local-first already answered (inference_mode=local): skip the CLI
        // entirely — the unconsumed cli_future is lazy and simply drops.
        Some(local_reply) => Ok(local_reply),
        None if is_channel_session => {
            crate::claude_runner::REPLY_CHANNEL
                .scope(session_id.to_string(), cli_future)
                .await
        }
        None => cli_future.await,
    };
    let reply = match reply {
        // Last-line defense: an empty "success" must NOT flow onward — the
        // channels all skip empty sends (user sees nothing) and an empty
        // assistant turn would be appended to the session, teaching the model
        // to keep answering with nothing (the "session chain break" bug).
        // Convert to the error path so the classified 空回應 fallback message
        // is sent and channel_failures.jsonl gets an audit row.
        Ok(reply) if reply.trim().is_empty() => {
            warn!("Reply pipeline returned empty response — routing to fallback chain");
            last_cli_error = Some("Empty response from reply pipeline".to_string());
            None
        }
        Ok(reply) => {
            if !local_first_answered {
                info!("Claude replied via Claude Code SDK ({} chars)", reply.len());
                // D7 recovery: a Claude account actually answered, so any
                // open "all accounts failed authentication" outage is over —
                // close it and say so once. Deliberately gated on
                // `!local_first_answered`: a local-model reply proves nothing
                // about cloud credentials and must not clear the alarm (that
                // would flap the notification on every alternating turn).
                let outage_agent = if agent_id.trim().is_empty() {
                    "system"
                } else {
                    agent_id.as_str()
                };
                crate::auth_outage::record_recovery(&ctx.home_dir, outage_agent).await;
            }
            Some(reply)
        }
        Err(e) => {
            let log_line = format!("[{}] claude CLI error: {e}\n", chrono::Utc::now());
            let _ = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(ctx.home_dir.join("debug.log"))
                .await
                .map(|mut f| {
                    use tokio::io::AsyncWriteExt;
                    tokio::spawn(async move {
                        let _ = f.write_all(log_line.as_bytes()).await;
                    });
                });
            warn!("claude CLI unavailable: {e}");
            last_cli_error = Some(e);
            None
        }
    };

    // 2. Fallback: Local model inference (if configured)
    let reply = match reply {
        Some(r) => Some(r),
        None if local_attempted_first => {
            // inference_mode=local already tried (and failed) local FIRST —
            // don't retry the same engine; proceed to the Direct API fallback.
            None
        }
        None => {
            match crate::claude_runner::try_local_inference(
                &ctx.home_dir,
                &sanitized_text,
                &full_system_prompt,
                local_model_id.as_deref(),
                Some(&agent_id),
                capabilities.as_ref(),
            )
            .await
            {
                Ok(local_reply) => {
                    info!("Replied via local model ({} chars)", local_reply.len());
                    // Prepend a notice so the user knows CLI failed and local model is answering
                    let cli_err = last_cli_error.as_deref().unwrap_or("unknown");
                    let hint = classify_cli_error_hint(cli_err);
                    let notice = format!(
                        "⚠️ Claude CLI 暫時不可用（{hint}），本次由本地模型代為回應。\n\
                         系統會在背景自動偵測恢復。\n\n"
                    );
                    Some(format!("{notice}{local_reply}"))
                }
                Err(e) => {
                    if e != "ROUTER_ESCALATE_TO_CLOUD" {
                        warn!("Local inference unavailable: {e}");
                    }
                    None
                }
            }
        }
    };

    // 3. Fallback: Direct Anthropic Messages API (Rust-native, no Python).
    //
    // The Direct API requires an API key — OAuth tokens are not supported.
    // Only attempt this fallback when an API key is available; skip entirely
    // for OAuth-only setups to avoid the misleading "未設定任何 API 帳號" error.
    let fallback_api_key = get_api_key(&ctx.home_dir).await;
    let reply = match reply {
        Some(r) => Some(r),
        // A `moa:` id is not an Anthropic model — the MoA branch above was
        // this request's API path; don't re-send the ensemble id upstream.
        None if fallback_api_key.is_some() && !duduclaw_llm::is_moa_model_id(&model) => {
            let key = fallback_api_key.as_deref().unwrap_or_default();
            // P34 #4: a `system_operator`-capable agent gets one attempt at
            // the real MCP tool loop before falling through to the plain
            // tools-less call below. `is_operator` false (every other agent)
            // ⇒ `operator_tool_reply` is `None` at zero extra cost, so this
            // whole branch stays byte-identical to before for non-operator
            // agents. See `try_operator_direct_api_tool_loop`'s doc comment
            // for the full rationale and fail-safe contract.
            let is_operator = capabilities
                .as_ref()
                .map(|c| c.system_operator)
                .unwrap_or(false);
            let operator_tool_reply = if is_operator {
                try_operator_direct_api_tool_loop(
                    &agent_id,
                    key,
                    &model,
                    &full_system_prompt,
                    &sanitized_text,
                    capabilities.as_ref(),
                )
                .await
            } else {
                None
            };
            if let Some(text) = operator_tool_reply {
                Some(text)
            } else {
                match crate::direct_api::call_direct_api(
                    key,
                    &model,
                    &full_system_prompt,
                    &sanitized_text,
                    &[],
                )
                .await
                {
                    Ok(resp) => {
                        info!("Claude replied via Direct API ({} chars)", resp.text.len());
                        Some(resp.text)
                    }
                    Err(e) => {
                        let log_line = format!("[{}] direct API error: {e}\n", chrono::Utc::now());
                        let _ = tokio::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(ctx.home_dir.join("debug.log"))
                            .await
                            .map(|mut f| {
                                use tokio::io::AsyncWriteExt;
                                tokio::spawn(async move {
                                    let _ = f.write_all(log_line.as_bytes()).await;
                                });
                            });
                        warn!("Direct API unavailable: {e}");
                        // Only overwrite if we don't already have a more specific CLI error.
                        if last_cli_error.is_none() {
                            last_cli_error = Some(e);
                        }
                        None
                    }
                }
            }
        }
        None => {
            info!("Skipping Direct API fallback — no API key available (OAuth-only setup)");
            None
        }
    };

    if let Some(mut reply) = reply {
        // ── Action-claim verifier (shadow mode) ─────────────────────
        //
        // Cross-reference factual assertions in `reply` against the
        // MCP tool-call audit trail (`tool_calls.jsonl`) that was
        // populated during this turn. Catches "Agnes-class" bugs where
        // the agent narrates having done something (created 12 agents,
        // sent a message, updated a SOUL file) without actually calling
        // the corresponding MCP tool.
        //
        // Currently runs in SHADOW MODE: detections are logged to the
        // security audit log and emitted as tracing events, but the
        // reply is NOT altered. This lets us gather a `ungrounded_claim_rate`
        // baseline before flipping to enforce mode.
        //
        // Zero LLM cost — pure regex + log diff.
        // Zero marginal latency — runs on a value we already have.
        if !agent_id.is_empty() {
            let hallucinations = duduclaw_security::action_claim_verifier::detect_hallucinations(
                &ctx.home_dir,
                &agent_id,
                &reply,
                &dispatch_start_time,
            );
            if !hallucinations.is_empty() {
                warn!(
                    agent = %agent_id,
                    session_id,
                    count = hallucinations.len(),
                    "🚨 Action-claim verifier flagged {} ungrounded claim(s) in reply (shadow mode — not blocking)",
                    hallucinations.len()
                );
                for h in &hallucinations {
                    if let duduclaw_security::action_claim_verifier::VerifyResult::Hallucination {
                        claim,
                        reason,
                    } = h
                    {
                        warn!(
                            agent = %agent_id,
                            claim_type = ?claim.claim_type,
                            target = %claim.target_id,
                            matched_text = %claim.matched_text,
                            reason = %reason,
                            "ungrounded claim"
                        );
                        // Append a structured entry to security_audit.jsonl
                        // so dashboards and forensic tooling can surface
                        // the event. One row per claim.
                        duduclaw_security::audit::log_tool_hallucination(
                            &ctx.home_dir,
                            &agent_id,
                            &claim.matched_text,
                            claim.claim_type.expected_tool(),
                        );
                        // C1 producer 甲 companion — see `security_autopilot.rs`.
                        crate::security_autopilot::emit_tool_hallucination(&agent_id);
                    }
                }
            }
        }

        // Record outbound for circuit breaker echo detection
        let reply_tokens = estimate_tokens(&reply);
        if let Some(ref cb_registry) = ctx.circuit_breakers {
            cb_registry
                .record_outbound(session_id, &reply, reply_tokens as usize)
                .await;
        }

        // Inject defensive prompt if circuit breaker is in HalfOpen (bot loop suspected)
        if crate::defensive_prompt::should_inject(breaker_state)
            && ctx.killswitch.defensive_prompt.enabled
        {
            // Extract channel type from session_id (e.g. "telegram:123" → "telegram")
            let channel_type = session_id.split(':').next().unwrap_or("unknown");
            reply = crate::defensive_prompt::inject_defensive_prompt(
                &reply,
                &ctx.killswitch.defensive_prompt.languages,
                channel_type,
            );
            debug!(
                session_id,
                "Defensive prompt injected (circuit breaker HalfOpen)"
            );
        }

        // Save only committed CCR provenance tied to this exact user turn.
        // The session stores metadata, never the original tool payload.
        if let Some(user_message_id) = user_message_id {
            if let Err(e) = crate::ccr_runtime::persist_collected_results(
                &session_mgr,
                &ctx.home_dir,
                &agent_id,
                session_id,
                user_message_id,
            )
            .await
            {
                warn!("Failed to save CCR references to session: {e}");
            }
        }

        // Save assistant reply to session.
        //
        // This runs BEFORE the CCR delivery lease is rechecked (that happens
        // in `GuardedReply::new`, after this whole function returns), so the
        // row id is registered with the turn's delivery gate: a refused
        // delivery revokes and overwrites it instead of leaving
        // source-derived wording in the conversation history.
        match session_mgr
            .append_message_with_id(session_id, "assistant", &reply, reply_tokens)
            .await
        {
            Ok(message_id) => record_turn_assistant_message(session_id, message_id),
            Err(e) => warn!("Failed to save assistant message to session: {e}"),
        }

        // Notify dashboard clients that a session gained a turn, so the
        // conversation sidebar re-lists without a webchat-local trigger.
        // Channel conversations (Telegram/Discord/…) otherwise stay
        // invisible until a full reload.
        {
            let event = crate::protocol::WsFrame::event(
                "chat.sessions.updated",
                serde_json::json!({
                    "session_id": session_id,
                    "agent_id": agent_id,
                }),
            );
            if let Ok(json) = serde_json::to_string(&event) {
                let _ = ctx.event_tx.send(json);
            }
        }

        // Trace the turn in the activity feed (agent detail 紀錄/即時動態).
        // Tier-3 in the dashboard feed, so it informs without flooding.
        {
            let (ch, _) = parse_session_id_parts(session_id);
            let summary = format!(
                "回覆 {} 對話「{}」",
                channel_display_name(ch),
                duduclaw_core::truncate_chars(&sanitized_text, 40),
            );
            let home = ctx.home_dir.clone();
            let tx = ctx.event_tx.clone();
            let aid = agent_id.clone();
            tokio::spawn(async move {
                post_conversation_activity(&home, &tx, &aid, "agent_reply", summary).await;
            });
        }

        // ── RFC-24: Decision Continuity capture (async, non-blocking) ──
        // When the outbound reply offers an enumerated choice ("方案 A/B/C",
        // "Option 1/2", a lettered list under a "which one?" question), persist
        // each option into the temporal/semantic store so a later "用方案 C"
        // (new turn / session / process, even after compress() destroys the
        // turn) still resolves from durable state instead of being guessed from
        // history. Opt-in per agent (`[memory] decision_continuity`); detection
        // is deterministic and best-effort — any failure here is logged and
        // never affects reply delivery. Uses ctx.memory_db_path directly (its
        // own opt-in flag).
        if agent_dir
            .as_deref()
            .map(crate::runtime_config::decision_continuity_enabled)
            .unwrap_or(false)
        {
            if let Some(db_path) = ctx.memory_db_path.clone() {
                let agent_for_dec = agent_id.clone();
                let source_msg = format!("{session_id}|{reply}");
                let reply_for_dec = reply.clone();
                let ttl_days = agent_dir
                    .as_deref()
                    .map(crate::runtime_config::decision_ttl_days)
                    .unwrap_or(7);
                let util_model = agent_dir
                    .as_deref()
                    .map(crate::runtime_config::agent_utility_model)
                    .unwrap_or_else(|| crate::runtime_config::DEFAULT_UTILITY_MODEL.to_string());
                let home_for_dec = ctx.home_dir.clone();
                let ctx_meta = {
                    let (ch, cid) = parse_session_id_parts(session_id);
                    serde_json::json!({ "channel": ch, "chat_id": cid, "session_id": session_id })
                };
                tokio::spawn(async move {
                    // TTL housekeeping always runs (cheap, self-pruning) — !Send engine.
                    {
                        let db = db_path.clone();
                        let a = agent_for_dec.clone();
                        let home_for_ttl = home_for_dec.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            // H4: single construction point.
                            if let Ok(engine) =
                                crate::memory_factory::build_memory_engine(&db, &home_for_ttl)
                            {
                                let rt = tokio::runtime::Handle::current();
                                if let Ok(n) =
                                    rt.block_on(engine.expire_stale_decisions(&a, ttl_days))
                                {
                                    if n > 0 {
                                        crate::metrics::global_metrics().decision_expired(n as u64);
                                    }
                                }
                            }
                        })
                        .await;
                    }

                    // P3.1: confident → zero-cost; suspected → one Haiku confirm;
                    // no-choice → done.
                    let draft = match crate::decision_capture::classify_outbound(&reply_for_dec) {
                        crate::decision_capture::DetectionResult::Confident(d) => d,
                        crate::decision_capture::DetectionResult::Suspected => {
                            let prompt =
                                crate::decision_capture::build_extraction_prompt(&reply_for_dec);
                            match call_claude_cli_lightweight(&prompt, &util_model, &home_for_dec)
                                .await
                            {
                                Ok(out) => {
                                    match crate::decision_capture::parse_extracted_decision(&out) {
                                        Some(d) => {
                                            tracing::info!(
                                                agent = %agent_for_dec,
                                                "RFC-24: suspected choice confirmed by Haiku second-pass"
                                            );
                                            d
                                        }
                                        None => return, // Haiku said not a decision
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "decision capture: Haiku second-pass failed");
                                    return;
                                }
                            }
                        }
                        crate::decision_capture::DetectionResult::NoChoice => return,
                    };

                    let id = crate::decision_capture::decision_id(&agent_for_dec, &source_msg);
                    // SqliteMemoryEngine is !Send (rusqlite) — persist on a blocking thread.
                    let _ = tokio::task::spawn_blocking(move || {
                        // H4: single construction point.
                        let engine = match crate::memory_factory::build_memory_engine(
                            &db_path,
                            &home_for_dec,
                        ) {
                            Ok(e) => e,
                            Err(e) => {
                                tracing::warn!(error = %e, "decision capture: open engine failed");
                                return;
                            }
                        };
                        let rt = tokio::runtime::Handle::current();
                        match rt.block_on(crate::decision_capture::persist_decision(
                            &engine,
                            &agent_for_dec,
                            &id,
                            &draft,
                            ctx_meta,
                        )) {
                            Ok(()) => {
                                crate::metrics::global_metrics().decision_captured();
                                tracing::info!(
                                    agent = %agent_for_dec,
                                    decision_id = %id,
                                    options = draft.options.len(),
                                    "RFC-24: decision captured"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "decision capture: persist failed")
                            }
                        }
                    })
                    .await;
                });
            }

            // RFC-24 §4.4 (P2.2) + §4.5 (P2.3): if THIS user message referenced a
            // decision ("用方案 C"), either auto-resolve the matching open decision
            // (so it stops re-injecting) OR — when no open decision matches (the
            // Agnes failure shape) — record a learning signal so F2 Reflexion
            // consolidates an anti-guessing rule. Background, best-effort.
            if let Some(db_path) = ctx.memory_db_path.clone() {
                let agent_for_res = agent_id.clone();
                let user_text = sanitized_text.clone();
                let nb_for_res = ctx.mistake_notebook.clone();
                let session_for_res = session_id.to_string();
                let home_for_res = ctx.home_dir.clone();
                let home_for_res_engine = home_for_res.clone();
                tokio::spawn(async move {
                    let _ = tokio::task::spawn_blocking(move || {
                        // H4: single construction point.
                        let engine = match crate::memory_factory::build_memory_engine(
                            &db_path,
                            &home_for_res_engine,
                        ) {
                            Ok(e) => e,
                            Err(e) => {
                                tracing::warn!(error = %e, "decision auto-resolve: open engine failed");
                                return;
                            }
                        };
                        let rt = tokio::runtime::Handle::current();
                        let open = rt
                            .block_on(engine.list_open_decisions(&agent_for_res, 20))
                            .unwrap_or_default();

                        if let Some((id, key)) =
                            crate::decision_capture::detect_decision_reference(&user_text, &open)
                        {
                            match rt.block_on(engine.resolve_decision(&agent_for_res, &id, &key)) {
                                Ok(duduclaw_memory::DecisionResolveOutcome::Resolved {
                                    chosen_key,
                                    ..
                                }) => {
                                    crate::metrics::global_metrics().decision_resolved();
                                    tracing::info!(
                                        agent = %agent_for_res,
                                        decision_id = %id,
                                        chosen = %chosen_key,
                                        "RFC-24: decision auto-resolved from user reference"
                                    );
                                }
                                Ok(other) => {
                                    tracing::debug!(?other, "decision auto-resolve: no-op outcome")
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "decision auto-resolve failed")
                                }
                            }
                            return;
                        }

                        // §4.5: user referenced a decision but NONE is open → the
                        // Agnes gap. Record a Capability mistake so F2 consolidates
                        // "don't guess referenced decisions — acknowledge + query".
                        if open.is_empty()
                            && crate::decision_capture::mentions_decision_reference(&user_text)
                        {
                            if let Some(nb) = nb_for_res {
                                let entry = crate::gvu::mistake_notebook::build_mistake_entry(
                                    &agent_for_res,
                                    &session_for_res,
                                    crate::gvu::mistake_notebook::MistakeCategory::Capability,
                                    &user_text,
                                    "(referenced decision had no durable record)",
                                    "使用者引用了某個方案/選項，但沒有任何未決決策可對應。\
                                     不可從歷史記錄模糊比對臆測；應承認缺漏並向使用者確認。",
                                    None,
                                    // WP2: RFC-24 decision-gap detections are a
                                    // distinct failure mode from general task
                                    // failures — counted separately so they
                                    // don't pool into the same consolidation.
                                    "decision_gap",
                                )
                                // B2b: `open.is_empty() && mentions_decision_reference(...)`
                                // above is a deterministic check, not an LLM
                                // self-report — always attach evidence.
                                .with_evidence(decision_gap_evidence(&user_text));
                                if let Err(e) = nb.record(&entry) {
                                    tracing::warn!(error = %e, "decision gap: record mistake failed");
                                } else {
                                    let _ = rt.block_on(crate::reflexion::maybe_consolidate(
                                        &nb,
                                        &db_path,
                                        &home_for_res,
                                        &agent_for_res,
                                        crate::gvu::mistake_notebook::MistakeCategory::Capability,
                                        crate::reflexion::DEFAULT_CONSOLIDATE_THRESHOLD,
                                    ));
                                    tracing::info!(
                                        agent = %agent_for_res,
                                        "RFC-24: recorded decision-gap learning signal (F2)"
                                    );
                                }
                            }
                        }
                    })
                    .await;
                });
            }
        }

        // ── RL trajectory collection (async, non-blocking) ─────────
        // Collect session as an RL training trajectory after each reply.
        // Runs in a background task to avoid adding latency to the hot path.
        {
            let home_for_rl = ctx.home_dir.clone();
            let sid_for_rl = session_id.to_string();
            let agent_for_rl = agent_id.clone();
            let model_for_rl = model.clone();
            let sm_for_rl = ctx.session_manager.clone();
            tokio::spawn(async move {
                let msgs = match sm_for_rl.get_messages(&sid_for_rl).await {
                    Ok(m) if !m.is_empty() => m,
                    Ok(_) => return,
                    Err(e) => {
                        tracing::debug!(error = %e, "RL collector: skip — cannot read session");
                        return;
                    }
                };
                let message_pairs: Vec<(String, String)> = msgs
                    .iter()
                    .map(|m| (m.role.clone(), m.content.clone()))
                    .collect();
                // Outcome reward: 1.0 for successful reply (we reached this code path)
                crate::rl::collector::collect_trajectory(
                    home_for_rl,
                    sid_for_rl,
                    agent_for_rl,
                    model_for_rl,
                    message_pairs,
                    1.0,
                )
                .await;
            });
        }

        // ── Instruction Pinning: extract on first turn ──────────────
        // Asynchronously extract core task instructions from the first user
        // message using Haiku (lightweight, same path as session compression).
        // Pinned instructions persist across turns and survive compression.
        if !has_history {
            let sm = ctx.session_manager.clone();
            let sid = session_id.to_string();
            let user_text = sanitized_text.clone();
            let home = ctx.home_dir.clone();
            tokio::spawn(async move {
                let prompt = format!(
                    "Extract the core task instructions from this user message. \
                     Output a concise bullet list of: goals, constraints, parameters, \
                     and deliverables. Max 200 words. Use the same language as the input.\n\n\
                     {user_text}"
                );
                match call_claude_cli_lightweight(
                    &prompt,
                    crate::runtime_config::DEFAULT_UTILITY_MODEL,
                    &home,
                )
                .await
                {
                    Ok(extracted) => {
                        if let Err(e) = sm.set_pinned(&sid, &extracted).await {
                            warn!(session_id = %sid, error = %e, "Failed to save pinned instructions");
                        } else {
                            info!(session_id = %sid, "Pinned task instructions extracted ({} chars)", extracted.len());
                        }
                    }
                    Err(e) => {
                        warn!(session_id = %sid, error = %e, "Instruction extraction failed (best-effort, non-blocking)");
                    }
                }
            });
        }

        // ── P2 Key-Fact Accumulator: extract facts from substantive turns ──
        // Only extracts when reply is long enough to contain useful information.
        // Async, non-blocking — same pattern as instruction extraction.
        if reply.len() > 100 {
            if let Some(db_path) = cognitive_memory_db.clone() {
                let agent_id_for_facts = agent_id.clone();
                let user_text_for_facts = sanitized_text.clone();
                let reply_snippet = duduclaw_core::truncate_bytes(&reply, 500).to_string();
                let (ch, cid) = parse_session_id_parts(session_id);
                let channel_for_facts = ch.to_string();
                let chat_id_for_facts = cid.to_string();
                let session_for_facts = session_id.to_string();
                let home_for_facts = ctx.home_dir.clone();
                let home_for_activity = ctx.home_dir.clone();
                let tx_for_activity = ctx.event_tx.clone();
                tokio::spawn(async move {
                    let prompt = format!(
                        "Extract 2-4 key factual insights from this conversation turn \
                         that would be useful in FUTURE conversations with this user. \
                         Focus on: user preferences, confirmed decisions, domain rules, \
                         technical constraints. Output bullet points only. Max 100 words. \
                         Same language as input.\n\n\
                         User: {user_text_for_facts}\n\
                         Assistant: {reply_snippet}"
                    );
                    let facts_text = match call_claude_cli_lightweight(
                        &prompt,
                        crate::runtime_config::DEFAULT_UTILITY_MODEL,
                        &home_for_facts,
                    )
                    .await
                    {
                        Ok(t) => t,
                        Err(e) => {
                            warn!(agent = %agent_id_for_facts, error = %e, "Key-fact extraction failed (best-effort)");
                            return;
                        }
                    };

                    // Store facts in spawn_blocking (SqliteMemoryEngine is !Send)
                    let agent_for_activity = agent_id_for_facts.clone();
                    let home_for_fact_store = home_for_facts.clone();
                    let stored = tokio::task::spawn_blocking(move || {
                        // H4: the key-fact auto-write is exactly the path
                        // `[memory] novelty_gate` exists to screen — build it
                        // through `memory_factory` so the gate is live.
                        let engine = match crate::memory_factory::build_memory_engine(
                            &db_path,
                            &home_for_fact_store,
                        ) {
                            Ok(e) => e,
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to open memory engine for fact storage");
                                return 0usize;
                            }
                        };
                        let rt = tokio::runtime::Handle::current();
                        let mut stored = 0usize;
                        for line in facts_text.lines() {
                            let fact = line.trim_start_matches(&['-', '•', '*', ' '][..]).trim();
                            if fact.len() < 10 { continue; }
                            // Dedup: check if similar fact already exists
                            if let Ok(existing) = rt.block_on(engine.search_facts(&agent_id_for_facts, fact, 1)) {
                                if existing.first().map_or(false, |e| duduclaw_memory::word_jaccard(&e.fact, fact) > 0.8) {
                                    let _ = rt.block_on(engine.bump_fact_access(&existing[0].id));
                                    continue;
                                }
                            }
                            if rt.block_on(engine.store_fact(
                                &agent_id_for_facts, fact,
                                &channel_for_facts, &chat_id_for_facts,
                                &session_for_facts,
                            )).is_ok() {
                                stored += 1;
                            }
                        }
                        stored
                    }).await.unwrap_or(0);

                    // Make the distillation visible: memory writes previously
                    // happened in total silence, which read as "沒有記憶".
                    if stored > 0 {
                        post_conversation_activity(
                            &home_for_activity,
                            &tx_for_activity,
                            &agent_for_activity,
                            "memory_distilled",
                            format!("從對話萃取 {stored} 筆關鍵事實（記憶 → 關鍵洞察）"),
                        )
                        .await;
                    }
                });
            }
        }

        // ── Prediction-driven evolution ──────────────────────────────
        // (BLOCKER R2-2) When the prediction engine is unconfigured the
        // spawned trust-feedback path below never runs, so citations
        // accumulate in the global `CitationTracker` until the 1-hour GC
        // reaps them — a slow memory leak under sustained traffic. Drain
        // the bucket synchronously here before deciding whether to proceed.
        if ctx.prediction_engine.is_none() {
            let _ = duduclaw_memory::feedback::global_tracker().drain(&turn_id);
        }
        if let Some(pe) = ctx.prediction_engine.as_ref() {
            let pe = pe.clone();
            let gvu = ctx.gvu_loop.clone();
            let user_id_for_pred = user_id.to_string();
            let agent_id_for_pred = agent_id.clone();
            let session_id_for_pred = session_id.to_string();
            let turn_id_for_pred = turn_id.clone();
            let text_clone = text.to_string();
            let reply_clone_for_pred = reply.clone();
            let home_for_pred = ctx.home_dir.clone();
            let agent_dir_for_pred = agent_dir.clone();
            let sm_for_pred = ctx.session_manager.clone();
            let skill_cache_for_pred = ctx.skill_cache.clone();
            let skill_activation_for_pred = ctx.skill_activation.clone();
            let skill_lift_for_pred = ctx.skill_lift.clone();
            let gap_acc_for_pred = ctx.gap_accumulator.clone();
            let sandbox_for_pred = ctx.sandbox_store.clone();
            // H2: the skill-synthesis follow-through reads the agent's
            // `[evolution]` knobs (default-off) from the registry.
            let registry_for_pred = ctx.registry.clone();
            let notebook_for_pred = ctx.mistake_notebook.clone();
            let memory_db_path_for_pred = cognitive_memory_db.clone();
            let injected_rule_ids_for_pred = injected_rule_ids.clone();
            let armed_shadow_for_pred = armed_shadow.clone();
            let held_out_gate_for_pred = held_out_gate_enabled;
            let ext_factors_cfg = external_factors_config.clone();
            let evolution_emitter_for_pred = ctx.evolution_emitter.clone();

            tokio::spawn(async move {
                // RAII drain guard — if anything below panics or returns
                // early, the citation tracker bucket for this turn is still
                // freed. (review HIGH R3-3.) The bus drains explicitly on
                // happy path; we disarm before that to avoid double-drain.
                let mut drain_guard =
                    duduclaw_memory::feedback::DrainOnDrop::new(turn_id_for_pred.clone());
                // 1. Generate prediction (< 1ms, zero LLM)
                let prediction = pe
                    .predict(&user_id_for_pred, &agent_id_for_pred, &text_clone)
                    .await;
                debug!(
                    agent = %agent_id_for_pred,
                    satisfaction = format!("{:.2}", prediction.expected_satisfaction),
                    confidence = format!("{:.2}", prediction.confidence),
                    "Prediction generated"
                );

                // 2. Extract conversation metrics
                let messages = sm_for_pred
                    .get_messages(&session_id_for_pred)
                    .await
                    .unwrap_or_default();
                let metrics = crate::prediction::metrics::ConversationMetrics::extract(
                    &session_id_for_pred,
                    &agent_id_for_pred,
                    &user_id_for_pred,
                    &messages,
                    0,
                );

                // 3. Calculate prediction error (embedding ~5ms if available, otherwise < 1ms)
                let (error, embedding) = pe.calculate_error(&prediction, &metrics).await;

                // 3.5 Log evolution event: PredictionError (Sutskever Day 1)
                pe.log_evolution_event(
                    "prediction_error",
                    &agent_id_for_pred,
                    Some(error.composite_error),
                    Some(&format!("{:?}", error.category)),
                    None,
                    None,
                    None,
                );

                // 4. Update user model — pass pre-computed embedding to avoid redundant embed()
                pe.update_model_with_embedding(&metrics, embedding).await;

                // 4.5 Conversation outcome detection + MistakeNotebook (Phase 1 GVU²)
                // Skip for very short conversations (< 4 messages) to avoid false positives (review #28)
                let mut error = error;
                let conv_outcome = if messages.len() >= 4 {
                    Some(crate::prediction::outcome::ConversationOutcome::extract(
                        &session_id_for_pred,
                        &agent_id_for_pred,
                        &messages,
                    ))
                } else {
                    None
                };
                // Apply task completion signal to prediction error
                if let Some(ref outcome) = conv_outcome {
                    let meta = pe.metacognition.lock().await;
                    error.apply_outcome(outcome, &meta.thresholds);
                }

                // ACE/ExpeL rule lifecycle: credit/blame the consolidated
                // rules injected into this turn's prompt using the settled
                // error category; net-zero rules are retired. Detached —
                // must run after apply_outcome so the category is final.
                //
                // Held-out gate on (v1.54): dialogue parity with
                // dispatch_engine's A4 settle — injected rules route through
                // the numeric-oracle gate instead of ErrorCategory credit,
                // and the shadow candidates armed at prompt-build time each
                // get their out-of-sample observation (the shadow-scoring
                // flow that lets an inductive lesson actually earn
                // promotion). Gate off ⇒ the unchanged `settle_detached`
                // runs, byte-identical.
                if let Some(ref dbp) = memory_db_path_for_pred {
                    if held_out_gate_for_pred {
                        if !injected_rule_ids_for_pred.is_empty()
                            || !armed_shadow_for_pred.ids.is_empty()
                        {
                            // Shadow-pass baseline: the agent's dialogue
                            // climatology (fraction of logged turns that were
                            // high-risk); domain-agnostic coin-flip until
                            // enough history accumulates — the same fallback
                            // shape as the task-layer pass.
                            let baseline = pe
                                .high_risk_base_rate(
                                    &agent_id_for_pred,
                                    crate::prediction::rule_gate::MIN_HELD_OUT_SAMPLES,
                                )
                                .await
                                .unwrap_or(crate::prediction::rule_gate::DEFAULT_BASELINE_HIT_RATE);
                            crate::prediction::rule_lifecycle::settle_detached_held_out(
                                dbp.clone(),
                                agent_id_for_pred.clone(),
                                injected_rule_ids_for_pred.clone(),
                                armed_shadow_for_pred.ids.clone(),
                                armed_shadow_for_pred.family_k,
                                error.category,
                                baseline,
                            );
                        }
                    } else {
                        crate::prediction::rule_lifecycle::settle_detached(
                            dbp.clone(),
                            agent_id_for_pred.clone(),
                            injected_rule_ids_for_pred.clone(),
                            error.category,
                        );
                    }
                }

                // ── Wiki RL trust feedback (Phase 2) ───────────────────
                // After the error is fully adjusted, dispatch to the trust
                // feedback bus so wiki pages cited during this turn get
                // their trust nudged up/down. Drains the citation tracker
                // for `turn_id_for_pred` (not session_id) so each turn's
                // citations are attributed only to its own prediction error.
                // (review B1)
                if let Some(bus) = crate::prediction::feedback_bus::TrustFeedbackBus::from_globals()
                {
                    let _ = bus.on_prediction_error(&turn_id_for_pred, &agent_id_for_pred, &error);
                } else {
                    // Trust store not initialised — drain tracker manually
                    // to keep memory bounded.
                    let _ = duduclaw_memory::feedback::global_tracker().drain(&turn_id_for_pred);
                }
                // Bus / fallback path drained the bucket — disarm the RAII
                // guard so it doesn't double-drain on scope exit.
                drain_guard.disarm();
                // Record failure to MistakeNotebook for grounded GVU
                if let Some(ref outcome) = conv_outcome {
                    if outcome.is_failure() {
                        if let Some(ref nb) = notebook_for_pred {
                            let category = match outcome.task_type {
                                crate::prediction::outcome::TaskType::Coding => {
                                    crate::gvu::mistake_notebook::MistakeCategory::Capability
                                }
                                crate::prediction::outcome::TaskType::QA => {
                                    crate::gvu::mistake_notebook::MistakeCategory::Factual
                                }
                                _ => crate::gvu::mistake_notebook::MistakeCategory::Behavioral,
                            };
                            let what_wrong = match outcome.satisfaction {
                                crate::prediction::outcome::SatisfactionSignal::Negative => {
                                    "User expressed dissatisfaction"
                                }
                                _ => "Task not completed",
                            };
                            let entry = crate::gvu::mistake_notebook::build_mistake_entry(
                                &agent_id_for_pred,
                                &session_id_for_pred,
                                category,
                                &text_clone,
                                &reply_clone_for_pred,
                                what_wrong,
                                None,
                                // WP2: general task-outcome failures are a
                                // separate failure mode from RFC-24
                                // decision-gap detections above.
                                "task_failure",
                            )
                            // B2b: `outcome` is the zero-LLM, pattern-matched
                            // `ConversationOutcome` (never the agent's
                            // self-report) — always attach evidence.
                            .with_evidence(conversation_outcome_evidence(outcome, &text_clone));
                            if let Err(e) = nb.record(&entry) {
                                warn!(agent = %agent_id_for_pred, "Failed to record mistake: {e}");
                            } else if let Some(ref dbp) = memory_db_path_for_pred {
                                // F2b: when this category accumulates ≥3 unresolved
                                // mistakes, consolidate them into a semantic memory
                                // rule. Detached so it never delays the reply path.
                                let nb2 = nb.clone();
                                let dbp2 = dbp.clone();
                                let aid2 = agent_id_for_pred.clone();
                                let home2 = home_for_pred.clone();
                                tokio::spawn(async move {
                                    match crate::reflexion::maybe_consolidate(
                                        &nb2,
                                        &dbp2,
                                        &home2,
                                        &aid2,
                                        category,
                                        crate::reflexion::DEFAULT_CONSOLIDATE_THRESHOLD,
                                    )
                                    .await
                                    {
                                        Ok(Some(id)) => info!(
                                            agent = %aid2, semantic_id = %id,
                                            "reflexion consolidated mistakes into semantic memory"
                                        ),
                                        Ok(None) => {}
                                        Err(e) => warn!(
                                            agent = %aid2,
                                            "reflexion consolidation failed: {e}"
                                        ),
                                    }
                                });
                            }
                        }
                    }
                }

                // WP1 master kill-switch: when `[evolution] enabled = false`,
                // freeze ALL autonomous evolution actions in the channel path
                // (skill diagnose/activate/synthesis/graduation in steps 5–6 and
                // the GVU trigger in step 7). Steps 1–4.5 above are pure
                // observation (prediction error logging, user-model update,
                // mistake recording) and still run so telemetry stays intact.
                let master_on = agent_dir_for_pred
                    .as_ref()
                    .map(|d| duduclaw_core::evolution_master_enabled(d))
                    .unwrap_or(true);

                // H3 (2026-09-29): publish this agent's `[evolution]` skill
                // knobs into the two process-wide controllers before they are
                // used this turn. `SkillActivationController` and
                // `GapAccumulator` live on `ChannelContext` (one per gateway)
                // while `max_active_skills` / `skill_synthesis_threshold` /
                // `skill_synthesis_cooldown_hours` are per-agent, so the
                // values have to be registered per agent rather than baked
                // into the constructor — which is exactly why they used to be
                // hard-coded `5` / `(3, 24)` and the dashboard's copies never
                // took effect. An agent absent from the registry registers
                // nothing and keeps the constructor defaults.
                let evo_knobs = {
                    let reg = registry_for_pred.read().await;
                    reg.get(&agent_id_for_pred).map(|a| {
                        (
                            a.config.evolution.max_active_skills,
                            a.config.evolution.skill_synthesis_threshold,
                            a.config.evolution.skill_synthesis_cooldown_hours,
                            a.config.evolution.skill_graduation_min_lift,
                        )
                    })
                };
                if let Some((max_active, syn_threshold, syn_cooldown, _)) = evo_knobs {
                    skill_activation_for_pred
                        .lock()
                        .await
                        .set_agent_max(&agent_id_for_pred, max_active);
                    gap_acc_for_pred.lock().await.set_agent_limits(
                        &agent_id_for_pred,
                        syn_threshold,
                        syn_cooldown,
                    );
                }

                // 5. Skill lifecycle: diagnose + activate + track lift
                if master_on {
                    let compressed: Vec<_> = {
                        let cache = skill_cache_for_pred.lock().await;
                        cache.all().into_iter().cloned().collect()
                    };

                    // Diagnose error and suggest skills
                    if let Some(diagnosis) =
                        crate::skill_lifecycle::diagnostician::diagnose(&error, &compressed)
                    {
                        // Activate suggested skills
                        if !diagnosis.suggested_skills.is_empty() {
                            let mut ctrl = skill_activation_for_pred.lock().await;
                            for skill_name in &diagnosis.suggested_skills {
                                let evicted = ctrl.activate(
                                    &agent_id_for_pred,
                                    skill_name,
                                    error.composite_error,
                                );
                                // Sprint N P0: emit skill_deactivate for capacity eviction (non-blocking)
                                // activate() returns the evicted skill name when max_active is reached.
                                if let Some(ref evicted_skill) = evicted {
                                    evolution_emitter_for_pred.emit_skill_deactivate(
                                        &agent_id_for_pred,
                                        evicted_skill,
                                        "capacity_eviction",
                                        serde_json::json!({
                                            "reason": "max_active_capacity_exceeded",
                                            "new_skill": skill_name,
                                        }),
                                    );
                                }
                                // Sprint N P0: emit skill_activate audit event (non-blocking)
                                evolution_emitter_for_pred.emit_skill_activate(
                                    &agent_id_for_pred,
                                    skill_name,
                                    "prediction_error_diagnosis",
                                );
                            }
                        }
                        // Report skill gap to evolution engine + accumulate for synthesis
                        if let Some(ref gap) = diagnosis.skill_gap {
                            crate::skill_lifecycle::gap::inject_skill_gap(
                                gap,
                                &home_for_pred,
                                &agent_id_for_pred,
                            );

                            // Accumulate gap for potential auto-synthesis
                            let trigger = {
                                let mut acc = gap_acc_for_pred.lock().await;
                                acc.record_gap(&agent_id_for_pred, gap, error.composite_error)
                            };
                            if let Some(trigger) = trigger {
                                info!(
                                    agent = %agent_id_for_pred,
                                    topic = %trigger.topic,
                                    gap_count = trigger.gap_count,
                                    "Skill synthesis trigger fired — queuing synthesis"
                                );
                                // Log synthesis trigger event to feedback.jsonl
                                // Use structured fields to prevent second-order injection via topic
                                let signal = serde_json::json!({
                                    "signal_type": "synthesis_trigger",
                                    "agent_id": &agent_id_for_pred,
                                    "topic": &trigger.topic,
                                    "gap_count": trigger.gap_count,
                                    "avg_composite_error": trigger.avg_composite_error,
                                    "channel": "skill_synthesis",
                                    "timestamp": chrono::Utc::now().to_rfc3339(),
                                });
                                let feedback_path = home_for_pred.join("feedback.jsonl");
                                let feedback_clone = feedback_path.clone();
                                let signal_str = signal.to_string();
                                // Non-blocking write to avoid stalling async runtime
                                tokio::task::spawn_blocking(move || {
                                    use std::io::Write;
                                    if let Err(e) = std::fs::OpenOptions::new()
                                        .create(true)
                                        .append(true)
                                        .open(&feedback_clone)
                                        .and_then(|mut f| writeln!(f, "{}", signal_str))
                                    {
                                        tracing::warn!(
                                            path = %feedback_clone.display(),
                                            error = %e,
                                            "Failed to write synthesis trigger to feedback.jsonl"
                                        );
                                    }
                                });

                                // Mark topic as pending to prevent re-triggering during
                                // async synthesis. The follow-through below calls
                                // confirm_synthesis() on success or cancel_pending()
                                // on every failure path, resuming gap accumulation.
                                {
                                    let mut acc = gap_acc_for_pred.lock().await;
                                    acc.mark_pending(&agent_id_for_pred, &trigger.topic);
                                }

                                // H2: consume the trigger. Detached so a slow
                                // synthesis never delays the reply path. Gated by
                                // `[evolution] skill_synthesis_enabled` (default
                                // false) — a stock install runs the `Disabled` arm,
                                // which only clears `pending` and spends nothing.
                                let acc_for_syn = gap_acc_for_pred.clone();
                                let sandbox_for_syn = sandbox_for_pred.clone();
                                let home_for_syn = home_for_pred.clone();
                                let agent_dir_for_syn = agent_dir_for_pred.clone();
                                let agent_for_syn = agent_id_for_pred.clone();
                                let registry_for_syn = registry_for_pred.clone();
                                tokio::spawn(async move {
                                    crate::skill_lifecycle::synthesis_runner::run(
                                        trigger,
                                        &home_for_syn,
                                        agent_dir_for_syn.as_deref(),
                                        &agent_for_syn,
                                        &registry_for_syn,
                                        &acc_for_syn,
                                        &sandbox_for_syn,
                                    )
                                    .await;
                                });
                            }
                        }
                    }

                    // Record conversation for activation effectiveness tracking
                    {
                        let mut ctrl = skill_activation_for_pred.lock().await;
                        ctrl.record_conversation(&agent_id_for_pred, error.composite_error);
                    }

                    // Track lift for each skill (active vs inactive)
                    {
                        let active = {
                            let ctrl = skill_activation_for_pred.lock().await;
                            ctrl.get_active(&agent_id_for_pred)
                        };
                        let mut lift_store = skill_lift_for_pred.lock().await;
                        for skill in &compressed {
                            let tracker = lift_store.get_or_create(&agent_id_for_pred, &skill.name);
                            if active.contains(&skill.name) {
                                tracker.record_with(error.composite_error);
                            } else {
                                tracker.record_without(error.composite_error);
                            }
                        }
                    }
                }

                // 6. Periodic: evaluate activations + scan distillation (every ~20 conversations)
                if master_on {
                    // Use prediction count as conversation counter (low overhead)
                    let should_evaluate = pe.metacognition.lock().await.total_predictions % 20 == 0;
                    if should_evaluate {
                        // Evaluate and prune ineffective skills
                        let deactivated = {
                            let mut ctrl = skill_activation_for_pred.lock().await;
                            ctrl.evaluate_all(&agent_id_for_pred)
                        };
                        for name in &deactivated {
                            info!(agent = %agent_id_for_pred, skill = %name, "Skill deactivated by effectiveness evaluation");
                            // Sprint N P0: emit skill_deactivate audit event (non-blocking)
                            evolution_emitter_for_pred.emit_skill_deactivate(
                                &agent_id_for_pred,
                                name,
                                "effectiveness_evaluation",
                                serde_json::json!({"reason": "prediction_error_not_improved"}),
                            );
                        }

                        // Scan for distillation candidates
                        let candidates = {
                            let lift_store = skill_lift_for_pred.lock().await;
                            let trackers = lift_store.get_all(&agent_id_for_pred);
                            crate::skill_lifecycle::distillation::scan_for_distillation(
                                &agent_id_for_pred,
                                &trackers,
                            )
                        };
                        for candidate in &candidates {
                            info!(
                                agent = %agent_id_for_pred,
                                skill = %candidate.skill_name,
                                readiness = format!("{:.2}", candidate.readiness),
                                lift = format!("{:.3}", candidate.lift),
                                "Skill ready for distillation into SOUL.md"
                            );
                            // Distillation via GVU would be triggered here in production
                            // (requires async GVU call — deferred to dedicated distillation task)
                        }

                        // Scan for graduation candidates (cross-agent migration)
                        {
                            let lift_store = skill_lift_for_pred.lock().await;
                            let trackers = lift_store.get_all(&agent_id_for_pred);
                            // H3: `[evolution] skill_graduation_min_lift` —
                            // previously `GraduationCriteria::default()`, so
                            // the dashboard's value was never consulted.
                            let criteria =
                                crate::skill_lifecycle::graduation::GraduationCriteria {
                                    min_lift: evo_knobs.map(|k| k.3).unwrap_or(
                                        crate::skill_lifecycle::graduation::GraduationCriteria::default()
                                            .min_lift,
                                    ),
                                    ..Default::default()
                                };
                            for tracker in &trackers {
                                if let Some(candidate) =
                                    crate::skill_lifecycle::graduation::check_graduation(
                                        tracker, &criteria,
                                    )
                                {
                                    info!(
                                        agent = %agent_id_for_pred,
                                        skill = %candidate.skill_name,
                                        lift = format!("{:.3}", candidate.lift),
                                        "Skill eligible for graduation to global scope"
                                    );
                                }
                            }
                        }

                        // Evaluate sandbox trials
                        // Lock ordering: collect data from each lock independently,
                        // never hold lift_store and sandbox_store simultaneously.
                        {
                            let sandbox_names = {
                                let store = sandbox_for_pred.lock().await;
                                store.active_names(&agent_id_for_pred)
                            };

                            // Collect tracker snapshots (lift data) — release lift_store before sandbox
                            let tracker_snapshots: Vec<_> = {
                                let lift_store = skill_lift_for_pred.lock().await;
                                sandbox_names
                                    .iter()
                                    .filter_map(|name| {
                                        lift_store
                                            .get_all(&agent_id_for_pred)
                                            .into_iter()
                                            .find(|t| t.skill_name == *name)
                                            .map(|t| (name.clone(), t.clone()))
                                    })
                                    .collect()
                            }; // lift_store released here

                            for (name, tracker) in &tracker_snapshots {
                                let sandboxed = {
                                    let store = sandbox_for_pred.lock().await;
                                    store.get(&agent_id_for_pred, name).cloned()
                                };
                                if let Some(sandboxed) = sandboxed {
                                    let outcome =
                                        crate::skill_lifecycle::sandbox_trial::evaluate_trial(
                                            tracker, &sandboxed,
                                        );
                                    match outcome.decision {
                                        crate::skill_lifecycle::sandbox_trial::TrialDecision::Graduate => {
                                            info!(agent = %agent_id_for_pred, skill = %name, "Sandbox trial → GRADUATE");
                                            let mut store = sandbox_for_pred.lock().await;
                                            store.graduate(&agent_id_for_pred, name);
                                        }
                                        crate::skill_lifecycle::sandbox_trial::TrialDecision::Discard => {
                                            info!(agent = %agent_id_for_pred, skill = %name, reason = %outcome.reason, "Sandbox trial → DISCARD");
                                            let mut store = sandbox_for_pred.lock().await;
                                            store.discard(&agent_id_for_pred, name);
                                            let mut ctrl = skill_activation_for_pred.lock().await;
                                            ctrl.deactivate(&agent_id_for_pred, name);
                                            // Sprint N P0: emit skill_deactivate audit event (non-blocking)
                                            evolution_emitter_for_pred.emit_skill_deactivate(
                                                &agent_id_for_pred,
                                                name,
                                                "sandbox_trial_discard",
                                                serde_json::json!({"reason": outcome.reason}),
                                            );
                                        }
                                        crate::skill_lifecycle::sandbox_trial::TrialDecision::ExtendTrial(extra) => {
                                            if extra > 0 {
                                                let mut store = sandbox_for_pred.lock().await;
                                                store.extend_ttl(&agent_id_for_pred, name, extra);
                                            }
                                        }
                                    }
                                }
                            }
                            // Tick all sandbox TTLs
                            let mut store = sandbox_for_pred.lock().await;
                            store.tick_agent(&agent_id_for_pred);
                        }
                    }
                }

                // 7. Route to evolution action (with hardening: ε-floor + anti-sycophancy)
                // Snapshot consistency first, then lock exploration (audit #1: avoid dual mutex)
                let consecutive = pe.consecutive_significant_count(&agent_id_for_pred).await;
                let consistency_snapshot = pe.consistency.lock().await.clone();
                // Master kill-switch: a frozen agent routes to `None` so neither
                // episodic-memory writes nor the GVU self-play loop fire.
                let action = if master_on {
                    let mut exploration = pe.exploration.lock().await;
                    crate::prediction::router::route(
                        &error,
                        consecutive,
                        &mut exploration,
                        &consistency_snapshot,
                    )
                } else {
                    crate::prediction::router::EvolutionAction::None
                };

                match action {
                    crate::prediction::router::EvolutionAction::None => {}
                    crate::prediction::router::EvolutionAction::StoreEpisodic {
                        content,
                        importance,
                    } => {
                        let preview: String = content.chars().take(80).collect();
                        debug!(agent = %agent_id_for_pred, "Storing episodic observation: {preview}");

                        // Persist to the shared `<home>/memory.db` — the same
                        // file every other production write path uses. This
                        // used to create `agents/<id>/state/memory.db`, which
                        // broke the invariant `handlers.rs::agent_memory_db_path`
                        // documents ("per-agent files only exist on old
                        // installs"): one stray episodic write here flipped
                        // every dashboard memory read RPC for the agent onto a
                        // near-empty per-agent file while key facts and rules
                        // kept accumulating, unseen, in the shared db (the
                        // 2026-08-20 關鍵洞察 empty-tab incident). Stray files
                        // already created are re-merged at boot by
                        // `memory_migrate::merge_per_agent_memory_dbs`.
                        if memory_db_path_for_pred.is_none() {
                            debug!(agent = %agent_id_for_pred, "Cognitive memory disabled — episodic observation not persisted");
                        } else if let Some(ref db_path) = memory_db_path_for_pred {
                            match crate::memory_factory::build_memory_engine(
                                db_path,
                                &home_for_pred,
                            ) {
                                Ok(engine) => {
                                    let entry = duduclaw_core::types::MemoryEntry {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        agent_id: agent_id_for_pred.clone(),
                                        content,
                                        timestamp: chrono::Utc::now(),
                                        tags: vec![],
                                        embedding: None,
                                        layer: duduclaw_core::types::MemoryLayer::Episodic,
                                        importance,
                                        access_count: 0,
                                        last_accessed: None,
                                        source_event: "prediction_episodic".to_string(),
                                    };
                                    // WP1: prediction-driven episodic writes are
                                    // agent self-derived; route through
                                    // store_temporal so the origin is bound.
                                    let ep_meta = duduclaw_memory::TemporalMeta {
                                        origin: Some("agent_derived".to_string()),
                                        ..Default::default()
                                    };
                                    match engine
                                        .store_temporal(&agent_id_for_pred, entry, ep_meta)
                                        .await
                                    {
                                        Err(e) => {
                                            warn!(agent = %agent_id_for_pred, "Failed to store episodic memory: {e}");
                                        }
                                        // WP6: this is the "對話餵資料 → 記憶"
                                        // path. Tell the dashboard so
                                        // MemoryBrowser refetches instead of
                                        // showing a stale list until reload.
                                        Ok(memory_id) => {
                                            crate::dashboard_feedback::emit(
                                                &home_for_pred,
                                                crate::dashboard_feedback::EV_MEMORY_CHANGED,
                                                serde_json::json!({
                                                    "action": "stored",
                                                    "agent_id": &agent_id_for_pred,
                                                    "memory_id": memory_id,
                                                }),
                                            )
                                            .await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(agent = %agent_id_for_pred, "Failed to open memory db: {e}");
                                }
                            }
                        }
                    }
                    crate::prediction::router::EvolutionAction::TriggerReflection {
                        ref context,
                    }
                    | crate::prediction::router::EvolutionAction::TriggerEmergencyEvolution {
                        ref context,
                    } => {
                        let is_emergency = matches!(
                            action,
                            crate::prediction::router::EvolutionAction::TriggerEmergencyEvolution { .. }
                        );
                        if is_emergency {
                            warn!(agent = %agent_id_for_pred, error = format!("{:.3}", error.composite_error), "Critical prediction error → emergency evolution");
                        } else {
                            info!(agent = %agent_id_for_pred, error = format!("{:.3}", error.composite_error), "Prediction error → triggering reflection");
                        }

                        // Log evolution event: GVU trigger (Sutskever Day 1)
                        let etype = if context.contains("Epistemic Foraging") {
                            "epistemic_foraging"
                        } else if context.contains("Anti-Sycophancy") {
                            "sycophancy_alert"
                        } else {
                            "gvu_trigger"
                        };
                        pe.log_evolution_event(
                            etype,
                            &agent_id_for_pred,
                            Some(error.composite_error),
                            Some(&format!("{:?}", error.category)),
                            Some(&context.chars().take(500).collect::<String>()),
                            None,
                            None,
                        );

                        // Enrich trigger context with external factors for Significant/Critical errors
                        let enriched_context = {
                            let ext = crate::external_factors::collect_external_factors(
                                &home_for_pred,
                                &agent_id_for_pred,
                                &ext_factors_cfg,
                            )
                            .await;
                            let ext_prompt = ext.to_prompt();
                            if ext_prompt.is_empty() {
                                context.clone()
                            } else {
                                format!("{context}\n\n{ext_prompt}")
                            }
                        };

                        // Sprint N P0 stub — signal suppression point for stagnation detection.
                        // TODO P1: replace `false` with real stagnation_detection threshold check.
                        //   P0 canonical stub metadata (Spec §1.1 — Option C, null placeholders):
                        //     { "suppressed_signal": null, "trigger_count": null, "window_seconds": null }
                        //   P1 example with real data (fill in actual values from stagnation config):
                        //   e.g.: if consecutive >= stagnation_cfg.trigger_threshold {
                        //       evolution_emitter_for_pred.emit_signal_suppressed_stub(
                        //           &agent_id_for_pred,
                        //           serde_json::json!({
                        //               "suppressed_signal": "prediction_error_diagnosis",
                        //               "trigger_count": consecutive,
                        //               "window_seconds": stagnation_cfg.window_seconds,
                        //           }),
                        //       );
                        //       // skip GVU trigger
                        //   }
                        let _signal_should_suppress = false; // always false in P0

                        // Run GVU loop if available
                        if let (Some(gvu), Some(dir)) = (&gvu, &agent_dir_for_pred) {
                            let contract = duduclaw_agent::contract::load_contract(dir);
                            let home = home_for_pred.clone();

                            // LLM caller: RFC-25 Phase 2 — route GVU evolution through
                            // the provider-agnostic choke-point so it honours the agent's
                            // [runtime] provider and [model] utility instead of forcing Claude.
                            let utility_model = crate::runtime_config::agent_utility_model(dir);
                            let call_llm = |prompt: String| {
                                let h = home.clone();
                                let d = dir.clone();
                                let aid = agent_id_for_pred.clone();
                                let model = utility_model.clone();
                                async move {
                                    crate::runtime_dispatch::run_agent_prompt_text(
                                        crate::runtime_dispatch::AgentPrompt {
                                            agent_dir: Some(&d),
                                            home_dir: &h,
                                            agent_id: &aid,
                                            prompt: &prompt,
                                            system_prompt: "",
                                            model: &model,
                                            max_tokens: 4096,
                                            provider_override: None,
                                            conversation_history: &[],
                                            request_type:
                                                crate::cost_telemetry::RequestType::Evolution,
                                            runtime_settings: None,
                                            effort: None,
                                            // P0/WP-B: ordinary caller — failover behavior unchanged.
                                            allow_cross_family_failover: true,
                                        },
                                    )
                                    .await
                                }
                            };

                            // Query MistakeNotebook for grounded generation context
                            let relevant_mistakes = notebook_for_pred
                                .as_ref()
                                .map(|nb| nb.query_by_agent(&agent_id_for_pred, 5))
                                .unwrap_or_default();

                            // WP0.3 (2026-08-06, root cause R4): ε-exploration / silence-timer
                            // triggers reach this branch without ever checking
                            // `category_warrants_gvu` (by design — exploration doesn't require
                            // a Significant/Critical error) but MUST still respect the
                            // per-agent opt-in toggle. The dispatcher path already enforces
                            // this via `trigger::maybe_run_gvu`; this channel path was the one
                            // caller that skipped it (see TODO-evolution-v3-2026-08.md WP0.3).
                            // Per-agent cooldown is enforced unconditionally inside
                            // `run_with_context` itself, so no separate check is needed here
                            // for that — this synthesizes the Skipped outcome BEFORE calling
                            // it so a disabled agent burns zero LLM budget (call_llm is never
                            // invoked in that branch).
                            let outcome = if !channel_gvu_trigger_allowed(dir) {
                                debug!(
                                    agent = %agent_id_for_pred,
                                    "GVU trigger routed via channel reply but \
                                     agent.toml [evolution] gvu_enabled = false — skipping"
                                );
                                crate::gvu::loop_::GvuOutcome::Skipped {
                                    reason: "agent.toml [evolution] gvu_enabled = false"
                                        .to_string(),
                                }
                            } else {
                                gvu.run_with_context(
                                    &agent_id_for_pred,
                                    dir,
                                    &enriched_context,
                                    &contract.boundaries.must_not,
                                    &contract.boundaries.must_always,
                                    call_llm,
                                    relevant_mistakes,
                                )
                                .await
                            };

                            // Log outcome and feed back to metacognition
                            match outcome {
                                crate::gvu::loop_::GvuOutcome::PlaybookEvolved {
                                    applied,
                                    ref verdict,
                                    ref entry_ids,
                                } => {
                                    info!(
                                        agent = %agent_id_for_pred,
                                        applied,
                                        %verdict,
                                        "AEE committed playbook deltas"
                                    );
                                    evolution_emitter_for_pred.emit_gvu_generation(
                                        &agent_id_for_pred,
                                        crate::evolution_events::schema::Outcome::Success,
                                        &etype,
                                        serde_json::json!({
                                            "gvu_outcome": "playbook_evolved",
                                            "applied": applied,
                                            "verdict": verdict,
                                            "entry_ids": entry_ids,
                                        }),
                                    );
                                    let mut meta = pe.metacognition.lock().await;
                                    meta.record_outcome(error.category, true);
                                }
                                crate::gvu::loop_::GvuOutcome::Abandoned { ref last_gradient } => {
                                    warn!(
                                        agent = %agent_id_for_pred,
                                        critique = %last_gradient.critique,
                                        "GVU abandoned all attempts"
                                    );
                                    // Sprint N P0: emit gvu_generation audit event (non-blocking)
                                    evolution_emitter_for_pred.emit_gvu_generation(
                                        &agent_id_for_pred,
                                        crate::evolution_events::schema::Outcome::Failure,
                                        &etype,
                                        serde_json::json!({"gvu_outcome": "abandoned", "critique": last_gradient.critique}),
                                    );
                                    let mut meta = pe.metacognition.lock().await;
                                    meta.record_outcome(error.category, false);
                                }
                                crate::gvu::loop_::GvuOutcome::Skipped { ref reason } => {
                                    debug!(agent = %agent_id_for_pred, reason, "GVU skipped");
                                    // Sprint N P0: emit gvu_generation audit event (non-blocking)
                                    evolution_emitter_for_pred.emit_gvu_generation(
                                        &agent_id_for_pred,
                                        crate::evolution_events::schema::Outcome::Failure,
                                        &etype,
                                        serde_json::json!({"gvu_outcome": "skipped", "reason": reason}),
                                    );
                                    // WP0.3: cooldown throttling and an explicit opt-out
                                    // (`gvu_enabled = false`) are deliberate non-runs, not
                                    // failed reflections — don't penalise metacognition for
                                    // either (same for an open AEE settlement window).
                                    if !reason.contains("settlement")
                                        && !reason.contains("cooldown")
                                        && !reason.contains("gvu_enabled")
                                    {
                                        let mut meta = pe.metacognition.lock().await;
                                        meta.record_outcome(error.category, false);
                                    }
                                }
                            }

                            // ── Proactive rule evaluation (post-GVU) ─────────
                            {
                                use duduclaw_agent::proactive::{
                                    RuleContext, RuleEvaluator, extract_proactive_rules,
                                };

                                let proactive_rules =
                                    extract_proactive_rules(&contract.boundaries.must_always);

                                if !proactive_rules.is_empty() {
                                    // Build context from available data.
                                    // hours_since_last_interaction: approximate from
                                    // conversation messages (last turn timestamp).
                                    let hours_since = {
                                        let msgs = sm_for_pred
                                            .get_messages(&session_id_for_pred)
                                            .await
                                            .unwrap_or_default();
                                        msgs.last()
                                            .and_then(|m| {
                                                chrono::DateTime::parse_from_rfc3339(&m.timestamp)
                                                    .ok()
                                                    .map(|ts| {
                                                        let elapsed = chrono::Utc::now()
                                                            - ts.with_timezone(&chrono::Utc);
                                                        (elapsed.num_seconds().max(0) as f32)
                                                            / 3600.0
                                                    })
                                            })
                                            .unwrap_or(0.0)
                                    };

                                    let recent_events: Vec<String> = Vec::new();
                                    let active_patterns: Vec<String> = Vec::new();

                                    let rule_ctx = RuleContext {
                                        hours_since_last_interaction: hours_since,
                                        recent_events,
                                        active_patterns,
                                    };

                                    let mut evaluator = RuleEvaluator::new();
                                    let triggered = evaluator.evaluate(&proactive_rules, &rule_ctx);

                                    for (rule, message) in &triggered {
                                        info!(
                                            agent = %agent_id_for_pred,
                                            rule = %rule.source_contract,
                                            "Proactive rule fired: {message}"
                                        );
                                    }

                                    if !triggered.is_empty() {
                                        debug!(
                                            agent = %agent_id_for_pred,
                                            count = triggered.len(),
                                            "Proactive rules evaluated post-GVU"
                                        );
                                    }
                                }
                            }
                        } else {
                            warn!(
                                agent = %agent_id_for_pred,
                                "Evolution triggered but GVU loop not available — skipping"
                            );
                        }
                    }
                }
            });
        }

        // ── Conversation distill (async, non-blocking) ───────────
        // WP5c: the pipeline routes into TWO sinks — durable reference
        // documents (charter / SOP / spec / policy) become an auto-filed page
        // under the agent's own `wiki/auto/` namespace plus one memory
        // pointer; everything else keeps going to the memory system with
        // temporal supersession. Human-curated wiki namespaces are never
        // written by this path. See wiki_ingest.rs module docs for the full
        // contract and the four isolation locks.
        //
        // Runs whenever a memory database is configured (D7: the cognitive
        // memory layer is always resident).
        //
        // `user_id` is threaded in for D9 (WP5d): the pipeline's first stage
        // routes self-stated preferences / forms of address / reply-style
        // requests into the per-user profile (`subject = user:<id>`) instead of
        // a generic semantic entry. `session_id` supplies the WP5c source
        // chain shown in the curation station ("Telegram 對話 · 8/4 10:12").
        if let Some(memory_db_for_distill) = cognitive_memory_db.clone() {
            let user_text_for_distill = sanitized_text.clone();
            let reply_for_distill = reply.clone();
            let agent_id_for_distill = agent_id.clone();
            let user_id_for_distill = user_id.to_string();
            let home_for_distill = ctx.home_dir.clone();
            let session_for_distill = session_id.to_string();
            // Wait for this turn's CCR delivery verdict before distilling: a
            // reply refused because its source lease was lost must not seed
            // memory or an auto-filed wiki page with source-derived wording.
            // `None` = no gate in scope (non-channel caller) ⇒ previous
            // behaviour; a dropped sender ⇒ skip (fail-closed).
            let delivery_verdict = ccr_delivery_verdict();
            tokio::spawn(async move {
                if let Some(verdict) = delivery_verdict {
                    if !matches!(verdict.await, Ok(true)) {
                        return;
                    }
                }
                crate::wiki_ingest::run_ingest(
                    &user_text_for_distill,
                    &reply_for_distill,
                    &agent_id_for_distill,
                    &user_id_for_distill,
                    &home_for_distill,
                    &memory_db_for_distill,
                    &session_for_distill,
                )
                .await;
            });
        }

        // ── Phase 3: Record trajectory for skill extraction ──────
        // Start or continue recording the conversation trajectory.
        // Recording is finalized when the next user message contains
        // positive/negative feedback (see "within 2 turns" check above).
        {
            let session_key = format!("{session_id}:{agent_id}");
            let mut recorder = ctx.skill_recorder.lock().await;
            if !recorder.is_recording(&session_key) {
                recorder.start(&session_key, &agent_id);
                recorder.record_turn(&session_key, "user", text, vec![]);
            }
            // Record the assistant reply turn
            // Tool names are not available here (streamed via CLI), so empty for now.
            // Future: parse tool_use events from streaming and pass them through.
            recorder.record_turn(&session_key, "assistant", &reply, vec![]);
        }

        // Check if compression needed; generate Claude summary then compress in background
        let sm = ctx.session_manager.clone();
        let sid = session_id.to_string();
        let home_for_compress = ctx.home_dir.clone();
        tokio::spawn(async move {
            if sm.should_compress(&sid).await {
                // Gather last messages to summarise
                let msgs = sm.get_messages(&sid).await.unwrap_or_default();
                let transcript = {
                    let mut buf = String::with_capacity(msgs.len() * 350);
                    for m in &msgs {
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        use std::fmt::Write;
                        // Byte-budget truncation must walk back to a char
                        // boundary (project rule #1: raw `&s[..n]` panics
                        // mid-char on CJK/emoji content).
                        let _ = write!(
                            buf,
                            "[{}] {}",
                            m.role,
                            duduclaw_core::truncate_bytes(&m.content, 300)
                        );
                    }
                    buf
                };
                let prompt = format!(
                    "Summarize the following conversation history concisely for use as context \
                     in future turns. Include key facts, decisions, and outcomes. Max 400 words.\n\n{transcript}"
                );
                let summary = match call_claude_cli_lightweight(
                    &prompt,
                    crate::runtime_config::DEFAULT_UTILITY_MODEL,
                    &home_for_compress,
                )
                .await
                {
                    Ok(s) => s,
                    Err(_) => {
                        "[Session compressed — previous conversation summary omitted for brevity]"
                            .to_string()
                    }
                };
                if let Err(e) = sm.compress(&sid, &summary).await {
                    warn!("Session compression failed: {e}");
                }
            }
        });

        // ── Goal intent router (P0) — append the confirmation menu (or
        // parse+strip the L2-B `<goal_suggest>` tag) on the way out. A
        // `GoalIntentAction::None` action — the overwhelming common case —
        // is a pure pass-through (one `to_string`, no other work). Runs
        // AFTER the background spawns above capture their own clones of the
        // pre-finalize `reply`, so wiki_ingest / skill_recorder see the raw
        // model output (including an unstripped `<goal_suggest>` tag on a
        // Gray-band hit) rather than the user-facing confirmation menu —
        // documented, low-severity P0 gap (the tag is routing metadata, not
        // secret data; follow-up would move this earlier if it matters).
        //
        // `text` (the raw function parameter), NOT `sanitized_text` — the
        // latter carries a `[user_id]\n` sender-metadata prefix
        // (`SENDER_PREFIX_OPEN`) that must never leak into a goal task's
        // description.
        let reply = crate::goal_intent::finalize(
            ctx,
            session_id,
            &agent_id,
            goal_intent_precheck.action,
            text,
            &reply,
        )
        .await;

        return reply;
    }

    // 3. Fallback: classified error message
    let reg = ctx.registry.read().await;
    let name = reg
        .main_agent()
        .map(|a| a.config.agent.display_name.clone())
        .unwrap_or_else(|| "DuDuClaw".to_string());
    drop(reg);

    let err_str = last_cli_error
        .clone()
        .unwrap_or_else(|| "No error info".to_string());
    let reason = classify_cli_failure(&err_str);
    warn!(
        agent = %name,
        reason = ?reason,
        last_error = %err_str.chars().take(200).collect::<String>(),
        "Channel reply fallback — all providers failed"
    );

    // D7 (2026-09-08 incident): an auth-class total failure is an outage, not
    // a bad minute — schedules and replies stay dead until a human pastes a
    // new token. Alarm once per outage; `record_outage` debounces and
    // swallows its own errors, so this can never turn a fallback message into
    // a hard failure.
    if reason == FailureReason::AuthFailed {
        let outage_agent = if agent_id.trim().is_empty() {
            "system"
        } else {
            agent_id.as_str()
        };
        crate::auth_outage::record_outage(&ctx.home_dir, outage_agent, &err_str).await;
    }

    // Append a structured audit line so the dashboard can surface failure trends.
    // R3: annotate with the MAST failure-taxonomy label (arXiv:2503.13657) —
    // deterministic from the FailureReason token + embedded diagnostics;
    // infra failures label `infra`, semantic ambiguity stays `unclassified`.
    let reason_token = format!("{reason:?}");
    let mast = crate::mast::classify(&crate::mast::FailureEvidence {
        reason: Some(&reason_token),
        error_text: Some(&err_str),
        ..Default::default()
    });
    // Stripe error-object pattern: attach "where to go look" alongside the
    // classification itself — console_url is the dashboard deep link (same
    // one the message text below surfaces), doc_url is the public docs page
    // when one actually exists (`failure_doc_url` never invents a URL).
    // `json!` serializes `Option<String>`/`Option<&str>` as `null` when
    // `None`, so this stays fail-quiet the same way `deep_link` itself does.
    let console_url = failure_console_url(&ctx.home_dir, reason);
    let doc_url = failure_doc_url(reason);
    let audit = serde_json::json!({
        "event": "channel_reply_fallback",
        "agent": name,
        "session_id": session_id,
        // W2-4: which platform the user got the fallback message on; `null`
        // for non-channel sessions.
        "channel": crate::trajectory_guard::channel_from_session_id(session_id),
        "reason": reason_token,
        "error": err_str.chars().take(300).collect::<String>(),
        "mast": mast.as_str(),
        "mast_category": mast.category_str(),
        "console_url": console_url,
        "doc_url": doc_url,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });
    if let Ok(line) = serde_json::to_string(&audit) {
        let path = ctx.home_dir.join("channel_failures.jsonl");
        if let Ok(mut f) = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            use tokio::io::AsyncWriteExt;
            let _ = f.write_all(format!("{line}\n").as_bytes()).await;
        }
    }

    format_fallback_message(&name, reason, &ctx.home_dir)
}

/// True when a non-Claude reply's actual answering runtime differs from the
/// agent's configured `[runtime] provider` — i.e. `execute_with_failover`
/// silently substituted a fallback runtime (primary CLI not registered /
/// unavailable, or primary execution failed and a fallback answered instead).
///
/// `requested` is `RuntimeType::as_str()` (e.g. `"grok"`); `actual` is
/// `RuntimeResponse::runtime_name` (e.g. `"claude"`). The SSE sub-mode of
/// `openai_compat` (`"openai_compat_sse"`) is normalized to `"openai_compat"`
/// first so it is never flagged as a substitution — it's the same provider,
/// just a different transport.
///
/// Pure and side-effect free so the substitution decision itself is unit
/// tested independent of the async plumbing that calls it.
pub(crate) fn is_runtime_substitution(requested: &str, actual: &str) -> bool {
    let normalized_actual = actual.strip_suffix("_sse").unwrap_or(actual);
    normalized_actual != requested
}

/// WP0.3 (2026-08-06, root cause R4): whether the channel-reply GVU trigger
/// path (ε-exploration / silence-timer, which deliberately bypasses
/// `category_warrants_gvu`) is allowed to invoke the GVU loop for this
/// agent. Thin, testable wrapper around the same `agent_gvu_enabled` gate
/// the dispatcher path already enforces via `trigger::maybe_run_gvu` — this
/// channel path was the one caller missing it
/// (`TODO-evolution-v3-2026-08.md` WP0.3). Fail-closed: missing file /
/// malformed TOML / absent key all deny (see `agent_gvu_enabled` doc).
pub(crate) fn channel_gvu_trigger_allowed(agent_dir: &std::path::Path) -> bool {
    crate::gvu::trigger::agent_gvu_enabled(agent_dir)
}

