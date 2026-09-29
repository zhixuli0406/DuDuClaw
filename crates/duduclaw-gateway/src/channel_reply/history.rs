use super::*;

/// Maximum character count for a single turn before it gets trimmed.
pub(super) const TURN_TRIM_THRESHOLD: usize = 800;
pub(super) const TURN_HEAD_CHARS: usize = 300;
pub(super) const TURN_TAIL_CHARS: usize = 200;

/// #12 glue (2026-05-12) — apply the prompt-compression pipeline when an
/// agent's `[budget] max_input_tokens` is set AND the request is over
/// budget. Returns either the compressed history (success) or the
/// original history (no budget configured / not over / pipeline failed),
/// plus a [`crate::prompt_compression::CompressionInfo`] describing what
/// (if anything) happened, for the caller to thread down to
/// `cost_telemetry`.
///
/// Why not propagate errors: the 200K cliff doubles input price but
/// doesn't break the call. Silent fallback preserves availability;
/// the `cost_pressure` event surfaces the regression for auditing.
///
/// WP5 (2607.12161) adds a cache-aware guard in front of the pipeline:
/// when the agent's trailing cache efficiency is healthy and the budget
/// overshoot is mild, compression is skipped outright, because rewriting
/// history in that regime tends to cost more (cache-prefix rebuild) than
/// it saves (fewer tokens). This is why the function is now `async` —
/// the guard needs a `cost_telemetry::summary_by_agent` read.
pub(super) async fn maybe_compress_history(
    system_prompt: &str,
    history: Vec<ConversationTurn>,
    user_message: &str,
    agent_id: &str,
) -> (
    Vec<ConversationTurn>,
    crate::prompt_compression::CompressionInfo,
) {
    // Look up the budget from cost_telemetry's cached agent config. We
    // can't get the LoadedAgent at this point without re-acquiring the
    // registry lock — instead let the per-agent budget read happen via
    // a small helper. Default 0 (= disabled) preserves prior behaviour.
    let budget = read_agent_budget_tokens(agent_id);
    if budget == 0 {
        return (
            history,
            crate::prompt_compression::CompressionInfo::default(),
        );
    }

    // Snapshot of the cost_pressure flag so the pipeline can pick a
    // more aggressive trim threshold for hot agents.
    let cost_pressure = crate::cost_telemetry::get_telemetry()
        .map(|t| t.is_under_cost_pressure(agent_id))
        .unwrap_or(false);

    // Convert ConversationTurn → OwnedChatMessage. Pipeline returns
    // OwnedChatMessage, which we map back below.
    let owned: Vec<crate::prompt_compression::OwnedChatMessage> = history
        .iter()
        .map(|t| crate::prompt_compression::OwnedChatMessage {
            role: t.role.clone(),
            content: t.content.clone(),
        })
        .collect();

    // WP5 cache-aware gate — fail-safe: any missing telemetry / config
    // simply falls through to the pipeline exactly like pre-WP5 behaviour.
    let home = duduclaw_core::duduclaw_home();
    let agent_dir = home.join("agents").join(agent_id);
    let guard_cfg = crate::prompt_compression::read_cache_guard_config(&agent_dir);
    if guard_cfg.min_eff > 0.0 {
        let views: Vec<crate::prompt_compression::ChatMessage<'_>> =
            owned.iter().map(|m| m.as_view()).collect();
        let estimated =
            crate::prompt_compression::estimate_request_tokens(system_prompt, &views, user_message);
        let overshoot = crate::prompt_compression::overshoot_ratio(estimated, budget);
        let cache_eff = match crate::cost_telemetry::get_telemetry() {
            Some(t) => t
                .summary_by_agent(agent_id, 1)
                .await
                .map(|s| s.summary.avg_cache_efficiency)
                .unwrap_or(0.0),
            None => 0.0,
        };
        if crate::prompt_compression::should_skip_for_cache(
            cache_eff,
            overshoot,
            guard_cfg.min_eff,
            guard_cfg.max_overshoot,
        ) {
            info!(
                agent_id,
                cache_eff,
                overshoot,
                estimated,
                budget,
                "prompt compression: cache-aware guard skipped pipeline \
                 (cache hot + mild overshoot — rebuilding the cache prefix \
                 would cost more than compression saves)"
            );
            crate::metrics::global_metrics().prompt_compression_skipped_cache_guard();
            return (
                history,
                crate::prompt_compression::CompressionInfo::default(),
            );
        }
    }

    match crate::prompt_compression::enforce_budget_traced(
        system_prompt,
        owned.clone(),
        user_message,
        budget,
        crate::prompt_compression::default_pipeline(),
        cost_pressure,
    ) {
        Ok((compressed, stages_ran)) => {
            for stage in &stages_ran {
                crate::metrics::global_metrics()
                    .prompt_compression_run(stage)
                    .await;
            }
            let info = crate::prompt_compression::CompressionInfo {
                compressed: !stages_ran.is_empty(),
                stages: stages_ran.join(","),
            };
            // If the pipeline didn't need to do anything (under budget),
            // it returns the input unchanged — caller doesn't care.
            let turns = compressed
                .into_iter()
                .map(|m| ConversationTurn {
                    role: m.role,
                    content: m.content,
                })
                .collect();
            (turns, info)
        }
        Err(exceeded) => {
            // Non-fatal degradation: log, emit a cost-pressure-like
            // signal, fall through with original history. This keeps
            // the call working at higher cost rather than mysteriously
            // failing. `compressed=false` in the returned info reflects
            // what's actually sent (the ORIGINAL history) — the
            // insufficient compressed version is discarded, not shipped.
            for stage in &exceeded.stages_tried {
                crate::metrics::global_metrics()
                    .prompt_compression_run(stage)
                    .await;
            }
            if !(exceeded.stages_tried.is_empty() && exceeded.protected_section_tokens > 0) {
                if let Some(pending) =
                    crate::prompt_compression::prepare_bisect_summary(&owned, cost_pressure)
                {
                    let generated = if pending.has_unprotected_text {
                        let prompt = crate::session_summarizer::format_summarization_prompt(
                            &pending.transcript,
                        );
                        crate::runtime_dispatch::run_utility_prompt(
                            &home,
                            Some(&agent_dir),
                            agent_id,
                            "",
                            &prompt,
                            crate::runtime_dispatch::UTILITY_MAX_TOKENS,
                        )
                        .await
                    } else {
                        Ok(String::new())
                    };
                    match generated {
                        Ok(summary) => {
                            if let Some(compressed) =
                                crate::prompt_compression::complete_bisect_summary(
                                    pending,
                                    &summary,
                                    system_prompt,
                                    user_message,
                                    budget,
                                )
                            {
                                crate::metrics::global_metrics()
                                    .prompt_compression_run("async_bisect_summary")
                                    .await;
                                let turns = compressed
                                    .into_iter()
                                    .map(|message| ConversationTurn {
                                        role: message.role,
                                        content: message.content,
                                    })
                                    .collect();
                                return (
                                    turns,
                                    crate::prompt_compression::CompressionInfo {
                                        compressed: true,
                                        stages: "async_bisect_summary".into(),
                                    },
                                );
                            }
                        }
                        Err(error) => tracing::warn!(agent_id, %error,
                            "budget enforcement: async summary fallback failed"),
                    }
                }
            }
            tracing::warn!(
                agent_id,
                estimated = exceeded.estimated_tokens,
                budget = exceeded.budget_tokens,
                stages = ?exceeded.stages_tried,
                "budget enforcement: compression pipeline insufficient; \
                 proceeding with full history (request will be expensive)"
            );
            (
                history,
                crate::prompt_compression::CompressionInfo::default(),
            )
        }
    }
}

/// Helper for [`maybe_compress_history`]. Reads
/// `agent.toml [budget] max_input_tokens` for the given agent. Returns
/// 0 (= disabled) on any failure, preserving v1.12.x behaviour for
/// agents that haven't opted in.
pub(super) fn read_agent_budget_tokens(agent_id: &str) -> u64 {
    // Resolve the agent dir from the gateway's home dir at runtime, via the
    // canonical DUDUCLAW_HOME resolver (single source of truth for the state
    // root) so this hot path can never drift back to a hardcoded ~/.duduclaw.
    let home = duduclaw_core::duduclaw_home();
    let agent_dir = home.join("agents").join(agent_id);
    crate::prompt_audit::read_max_input_tokens(&agent_dir).unwrap_or(0)
}

/// Trim a turn's content if it exceeds the threshold (char-level, CJK-safe).
///
/// Preserves the first and last portions, replacing the middle with a
/// "[trimmed N chars]" placeholder. Zero LLM cost — pure text surgery.
pub(super) fn trim_turn_content(content: &str) -> String {
    let char_count = content.chars().count();
    if char_count <= TURN_TRIM_THRESHOLD {
        return content.to_string();
    }
    // char-level slicing to avoid panic on multi-byte UTF-8 (CJK)
    let head: String = content.chars().take(TURN_HEAD_CHARS).collect();
    let tail: String = content.chars().skip(char_count - TURN_TAIL_CHARS).collect();
    let trimmed = char_count - TURN_HEAD_CHARS - TURN_TAIL_CHARS;
    format!("{}…\n[trimmed {} chars]\n…{}", head, trimmed, tail)
}

/// Format conversation history as an XML-delimited prompt prefix.
///
/// Used by CLI-based runtimes (Gemini, Codex) and as a fallback for Claude CLI
/// when `--resume` is unavailable (e.g., account rotation changed session store).
///
/// Applies token-reduction optimizations:
/// - Long turns (>800 chars) are trimmed with head/tail preservation
/// - Keeps conversation structure intact while reducing token usage
pub(crate) fn format_history_as_prompt(
    history: &[ConversationTurn],
    current_message: &str,
) -> String {
    if history.is_empty() {
        return current_message.to_string();
    }
    let mut buf = String::with_capacity(history.len() * 200 + current_message.len() + 64);
    buf.push_str("<conversation_history>\n");
    for turn in history {
        let content = trim_turn_content(&turn.content);
        // Escape closing tags in content to prevent XML structure corruption
        let safe_content = content
            .replace("</user>", "&lt;/user&gt;")
            .replace("</assistant>", "&lt;/assistant&gt;");
        buf.push('<');
        buf.push_str(&turn.role);
        buf.push('>');
        buf.push_str(&safe_content);
        buf.push_str("</");
        buf.push_str(&turn.role);
        buf.push_str(">\n");
    }
    buf.push_str("</conversation_history>\n\n");
    // Joanna field report: a bare "hi" after a heavy task turn re-triggered
    // the task's Drive searches for minutes — with no framing, an eager
    // persona treats unfinished history as a standing work order. Make the
    // contract explicit: history is context, the current message is the job.
    buf.push_str(
        "以上 <conversation_history> 只是過去的對話紀錄（上下文），其中的任務都已結束或暫停。\
         只回應下方 <current_message>；除非使用者現在明確要求，否則不要自行重啟、繼續或補做歷史中的任務，\
         也不要為了寒暄或簡短訊息呼叫工具。\n\n<current_message>\n",
    );
    buf.push_str(current_message);
    buf.push_str("\n</current_message>");
    buf
}

