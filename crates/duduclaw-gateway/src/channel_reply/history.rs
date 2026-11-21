use super::*;

/// Maximum character count for a single turn before it gets trimmed.
pub(super) const TURN_TRIM_THRESHOLD: usize = 800;
pub(super) const TURN_HEAD_CHARS: usize = 300;
pub(super) const TURN_TAIL_CHARS: usize = 200;

/// #12 glue (2026-05-12) — apply the prompt-compression pipeline when an
/// agent's `[budget] max_input_tokens` is set AND the request is over
/// budget. Returns either the compressed history (success) or the
/// byte-capped history (no budget configured / not over / pipeline failed),
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
    system_prompt: &mut String,
    history: Vec<ConversationTurn>,
    user_message: &str,
    agent_id: &str,
    session_id: &str,
) -> (
    Vec<ConversationTurn>,
    crate::prompt_compression::CompressionInfo,
) {
    let home = duduclaw_core::duduclaw_home();
    let budget = read_agent_budget_tokens(agent_id);
    let (history, info) = compress_history_with_budget(
        system_prompt, history, user_message, agent_id, session_id, home, budget,
    ).await;
    (crate::runtime::cap_conversation_history(&history), info)
}

async fn compress_history_with_budget(
    system_prompt: &mut String,
    history: Vec<ConversationTurn>,
    user_message: &str,
    agent_id: &str,
    session_id: &str,
    home: std::path::PathBuf,
    budget: u64,
) -> (Vec<ConversationTurn>, crate::prompt_compression::CompressionInfo) {
    // Cap before every early return and before copying native API turns.
    let history = crate::runtime::cap_conversation_history(&history);
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
            // Compression failure retains the already byte-capped history.
            // The insufficient compressed proposal is never shipped.
            for stage in &exceeded.stages_tried {
                crate::metrics::global_metrics()
                    .prompt_compression_run(stage)
                    .await;
            }
            if !(exceeded.stages_tried.is_empty() && exceeded.protected_section_tokens > 0)
                && crate::session_summarizer::summary_retry_ready_async(&home, session_id, "bisect").await {
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
                                crate::session_summarizer::summary_attempt_finished_async(&home, session_id, "bisect", None).await;
                                let compressed = extract_summary_system_context(system_prompt, compressed);
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
                            let reason = crate::session_summarizer::validate_summary_output(&summary)
                                .err().unwrap_or(crate::session_summarizer::SummaryFailure::Budget);
                            crate::session_summarizer::summary_attempt_finished_async(&home, session_id, "bisect", Some(reason)).await;
                        }
                        // RM-205: unavailable is a quiet skip (byte-capped history kept).
                        Err(error) if crate::runtime_config::is_utility_unavailable(&error) => crate::session_summarizer::summary_attempt_finished_async(
                            &home, session_id, "bisect", Some(crate::session_summarizer::SummaryFailure::Unavailable)).await,
                        Err(_error) => crate::session_summarizer::summary_attempt_finished_async(
                            &home, session_id, "bisect", Some(crate::session_summarizer::SummaryFailure::Utility)).await,
                    }
                }
            }
            tracing::warn!(
                agent_id,
                estimated = exceeded.estimated_tokens,
                budget = exceeded.budget_tokens,
                stages = ?exceeded.stages_tried,
                "budget enforcement: compression pipeline insufficient; \
                 proceeding with byte-capped history"
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
pub(crate) const PENDING_HISTORY_EXCEPTION: &str =
    "歷史框架的例外：只有此待確認清單非空，且本輪訊息是確認、取消或指涉其中項目時，才延續該項目，使用清單中的原始編號，不要重建草稿。仍須遵守工具自己的核准規則；本輪工具未回報寄送成功，絕不能宣稱已寄出。\n";

pub(crate) fn format_history_as_prompt(
    history: &[ConversationTurn],
    current_message: &str,
) -> String {
    let history = crate::runtime::cap_conversation_history(history);
    if history.is_empty() {
        return current_message.to_string();
    }
    let mut buf = String::with_capacity(history.len() * 200 + current_message.len() + 64);
    buf.push_str("<conversation_history>\n");
    for turn in &history {
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


/// Move intermediate system context out of the conversation-turn transport.
fn extract_summary_system_context(
    system: &mut String,
    messages: Vec<crate::prompt_compression::OwnedChatMessage>,
) -> Vec<crate::prompt_compression::OwnedChatMessage> {
    messages.into_iter().filter_map(|message| {
        if message.role == "system" {
            system.push_str("\n\n");
            system.push_str(&message.content);
            None
        } else { Some(message) }
    }).collect()
}

#[cfg(test)]
mod summary_context_tests {
    #[test]
    fn summary_is_system_context_not_a_conversation_turn() {
        let mut system = "fixed system".to_owned();
        let messages = vec![
            crate::prompt_compression::OwnedChatMessage { role: "system".into(),
                content: crate::session_summarizer::summary_system_context("- previous facts") },
            crate::prompt_compression::OwnedChatMessage { role: "user".into(), content: "latest".into() },
        ];
        let turns = super::extract_summary_system_context(&mut system, messages);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].content, "latest");
        assert!(system.contains("Do not mention, quote, or comment"));
        assert!(system.contains("- previous facts"));
    }
}

#[cfg(test)]
mod history_byte_cap_tests {
    use super::*;
    use crate::runtime::{cap_conversation_history, CONVERSATION_HISTORY_MAX_BYTES};

    fn turns(count: usize, repeats: usize) -> Vec<ConversationTurn> {
        (0..count).map(|index| ConversationTurn {
            role: if index % 2 == 0 { "user" } else { "assistant" }.into(),
            content: format!("{index}:{}最新結論", "中文🙂".repeat(repeats)),
        }).collect()
    }

    fn payload(history: &[ConversationTurn]) -> Vec<(&str, &str)> {
        history.iter().map(|t| (t.role.as_str(), t.content.as_str())).collect()
    }

    async fn failed_compression_in_backoff(session: &str) {
        let home = tempfile::tempdir().unwrap();
        crate::session_summarizer::summary_attempt_finished_async(
            home.path(), session, "bisect", Some(crate::session_summarizer::SummaryFailure::Utility),
        ).await;
        assert!(!crate::session_summarizer::summary_retry_ready_async(home.path(), session, "bisect").await);
        // Native OpenAI-compatible history carries full content, including CJK.
        let history = turns(20, 1200);
        let expected = cap_conversation_history(&history);
        // The system prompt alone exceeds the configured budget, ensuring that
        // pure compression fails and that back-off skips the utility call.
        let mut system = "system context ".repeat(1000);
        let current = "這次的使用者訊息🙂".repeat(100);
        let original_current = current.clone();
        let (actual, info) = compress_history_with_budget(
            &mut system, history.clone(), &current, "cap-test", session,
            home.path().to_owned(), 1,
        ).await;
        assert!(!info.compressed, "must exercise failed-compression fallback");
        assert!(actual.iter().map(|t| t.content.len()).sum::<usize>() <= CONVERSATION_HISTORY_MAX_BYTES);
        assert_eq!(payload(&actual), payload(&expected));
        assert!(actual.len() < history.len());
        assert_eq!(payload(&actual), payload(&history[history.len() - actual.len()..]));
        assert!(actual.iter().all(|t| std::str::from_utf8(t.content.as_bytes()).is_ok()));
        let (disabled, _) = compress_history_with_budget(
            &mut system, history.clone(), &current, "cap-test", session,
            home.path().to_owned(), 0,
        ).await;
        assert_eq!(payload(&disabled), payload(&expected));
        assert_eq!(current, original_current);
        assert!(format_history_as_prompt(&actual, &current)
            .ends_with(&format!("<current_message>\n{current}\n</current_message>")));
    }

    #[tokio::test]
    async fn native_cjk_history_is_capped_during_failed_summary_backoff() {
        failed_compression_in_backoff("webchat:large-native-history").await;
    }

    #[tokio::test]
    async fn public_visitor_cjk_history_is_capped_during_failed_summary_backoff() {
        failed_compression_in_backoff("odoolivechat:public-visitor").await;
    }

    #[tokio::test]
    async fn small_history_is_byte_identical_with_compression_disabled() {
        let home = tempfile::tempdir().unwrap();
        let history = turns(3, 2);
        let (actual, _) = compress_history_with_budget(
            &mut String::new(), history.clone(), "current", "cap-test", "small",
            home.path().to_owned(), 0,
        ).await;
        assert_eq!(payload(&actual), payload(&history));
        assert!(cap_conversation_history(&[]).is_empty());
    }

    #[test]
    fn oversized_newest_turn_keeps_utf8_tail_and_omits_older_turns() {
        // Exercise every alignment around a multi-byte character at the cap.
        for extra in 0..7 {
            let history = turns(2, CONVERSATION_HISTORY_MAX_BYTES + extra);
            let actual = cap_conversation_history(&history);
            assert_eq!(actual.len(), 1);
            assert_eq!(actual[0].role, history[1].role);
            assert!(actual[0].content.len() <= CONVERSATION_HISTORY_MAX_BYTES);
            assert!(actual[0].content.len() >= CONVERSATION_HISTORY_MAX_BYTES - 3);
            assert!(history[1].content.ends_with(&actual[0].content));
            assert!(actual[0].content.ends_with("最新結論"));
        }
    }

    #[test]
    fn whole_turn_suffix_stops_at_first_turn_that_does_not_fit() {
        let history = vec![
            ConversationTurn { role: "user".into(), content: "old small".into() },
            ConversationTurn { role: "assistant".into(), content: "x".repeat(CONVERSATION_HISTORY_MAX_BYTES) },
            ConversationTurn { role: "user".into(), content: "newest".into() },
        ];
        let actual = cap_conversation_history(&history);
        assert_eq!(payload(&actual), payload(&history[2..]));
        let exact = vec![ConversationTurn { role: "user".into(), content: "x".repeat(CONVERSATION_HISTORY_MAX_BYTES) }];
        assert_eq!(payload(&cap_conversation_history(&exact)), payload(&exact));
    }
}

#[cfg(test)]
mod pending_frame_tests {
    use super::*;
    #[test]
    fn absent_pending_block_preserves_history_frame_byte_for_byte() {
        let history = vec![ConversationTurn { role: "assistant".into(), content: "old reply".into() }];
        let actual = format_history_as_prompt(&history, "好");
        let expected = "<conversation_history>\n<assistant>old reply</assistant>\n</conversation_history>\n\n以上 <conversation_history> 只是過去的對話紀錄（上下文），其中的任務都已結束或暫停。只回應下方 <current_message>；除非使用者現在明確要求，否則不要自行重啟、繼續或補做歷史中的任務，也不要為了寒暄或簡短訊息呼叫工具。\n\n<current_message>\n好\n</current_message>";
        assert_eq!(actual, expected);
        assert_eq!(crate::pending_items::with_reminder(actual.clone(), None), expected);
        let with_pending = crate::pending_items::with_reminder(actual, Some(PENDING_HISTORY_EXCEPTION.into()));
        assert!(with_pending.starts_with(expected));
        assert!(with_pending.ends_with(PENDING_HISTORY_EXCEPTION));
    }
}
