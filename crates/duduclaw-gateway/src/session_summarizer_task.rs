//! Background task that drives the async summarization policy (#13).
//!
//! Every ten minutes, the pure policy in [`crate::session_summarizer`] picks
//! eligible sessions within its per-tick quota. This task partitions protected
//! text out of each transcript, asks the session owner's utility runtime for
//! a summary, and persists the summary plus capped verbatim protected text.
//! Only absent owners use global settings; invalid stored owners are refused. Each session
//! fails independently and keeps its prior summary when utility work fails.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::prompt_compression::{partition_turns_for_summary};
use crate::session::SessionManager;
use crate::session_summarizer::{
    SummarizeDecision, SummarizeParams, decide_summarization,
};

/// How often the task wakes up.
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(600); // 10 min

/// Background task handle. Spawning is fire-and-forget — the runtime
/// drops the handle.
pub fn spawn_summarizer(
    session_manager: Arc<SessionManager>,
    home_dir: PathBuf,
    params: SummarizeParams,
) -> tokio::task::JoinHandle<()> {
    spawn_summarizer_with_interval(session_manager, home_dir, params, DEFAULT_TICK_INTERVAL)
}

/// Like `spawn_summarizer` but takes a custom interval — used by tests
/// to avoid sleeping for 10 minutes.
pub fn spawn_summarizer_with_interval(
    session_manager: Arc<SessionManager>,
    home_dir: PathBuf,
    params: SummarizeParams,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            min_new_turns = params.min_new_turns_to_trigger,
            cooldown_secs = params.cooldown_seconds,
            max_per_tick = params.max_per_tick,
            "session summarizer task started"
        );

        let mut ticker = tokio::time::interval(interval);
        // First tick fires immediately — skip it so we wait `interval`
        // before the first run (avoids surprise on cold start).
        ticker.tick().await;

        loop {
            ticker.tick().await;
            tick_once(&session_manager, &home_dir, &params).await;
        }
    })
}

/// One iteration of the task — pulled out as `pub(crate)` so tests
/// can invoke it directly without scheduling.
pub(crate) async fn tick_once(
    session_manager: &SessionManager,
    home_dir: &std::path::Path,
    params: &SummarizeParams,
) {
    let candidates = match session_manager.list_summary_candidates().await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "summarizer: list_summary_candidates failed");
            return;
        }
    };
    if candidates.is_empty() {
        debug!("summarizer: no sessions in store, nothing to do");
        return;
    }

    // Filter before quota allocation so a cooling-down failure cannot starve
    // another session. Oldest successful summaries go first.
    let mut ready = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if crate::session_summarizer::summary_retry_ready_async(home_dir, &candidate.session_id, "async").await {
            ready.push(candidate);
        }
    }
    let mut candidates = ready;
    candidates.sort_by_key(|c| std::cmp::Reverse(c.seconds_since_last_summary.unwrap_or(u64::MAX)));
    let decisions = decide_summarization(&candidates, params);
    let to_run: Vec<(String, u32)> = decisions
        .into_iter()
        .filter_map(|(session_id, d)| match d {
            SummarizeDecision::SummarizeUpTo { turn } => Some((session_id, turn)),
            SummarizeDecision::Skip { .. } => None,
        })
        .collect();

    if to_run.is_empty() {
        debug!(
            candidates = candidates.len(),
            "summarizer: no sessions met summarization threshold this tick"
        );
        return;
    }

    info!(
        scheduled = to_run.len(),
        candidates = candidates.len(),
        "summarizer: dispatching this tick"
    );

    for (session_id, through_turn) in to_run {
        let Some(candidate) = candidates.iter().find(|c| c.session_id == session_id) else {
            continue;
        };
        match crate::autonomous_runtime::scope(
            home_dir, &candidate.agent_id, &[],
            summarize_one(session_manager, home_dir, &session_id, through_turn, params.min_new_turns_to_trigger),
        ).await {
            Ok(None) => {
                debug!(session_id, "summarizer: current snapshot no longer needs summarization");
            }
            Ok(Some(bytes)) => {
                crate::session_summarizer::summary_attempt_finished_async(home_dir, &session_id, "async", None).await;
                info!(
                session_id = %session_id,
                through_turn,
                summary_bytes = bytes,
                "summarizer: persisted summary"
                );
            }
            Err(e) => {
                crate::session_summarizer::summary_attempt_finished_async(home_dir, &session_id, "async", Some(e)).await;
                warn!(
                session_id = %session_id,
                through_turn,
                reason = ?e,
                "summarizer: session-level failure (continuing with next)"
                );
            }
        }
    }
}

/// Roll the previous summary forward through `through_turn` and persist the
/// result. Returns its byte length, or None when the current snapshot is ineligible.
async fn summarize_one(
    session_manager: &SessionManager,
    home_dir: &std::path::Path,
    session_id: &str,
    through_turn: u32,
    min_new_turns: u32,
) -> Result<Option<usize>, crate::session_summarizer::SummaryFailure> {
    use crate::session_summarizer::SummaryFailure;
    let snapshot = session_manager.history_snapshot(session_id).await
        .map_err(|_| SummaryFailure::ReadState)?;
    let expected_ids = snapshot.message_ids;
    let after = snapshot.summarized_through_turn;
    let checked = crate::session_summarizer::checked_summary_bookmark(
        session_id, &snapshot.rolling_summary, after, snapshot.messages.len());
    let through_turn = (through_turn as usize).min(snapshot.messages.len().saturating_sub(3)) as u32;
    if through_turn as usize <= checked.after
        || snapshot.messages.len().saturating_sub(checked.after) < min_new_turns as usize {
        return Ok(None);
    }
    let turns: Vec<_> = snapshot.messages.into_iter()
        .take(through_turn as usize).skip(checked.after)
        .map(|message| (message.role, message.content)).collect();
    let turns: Vec<_> = turns.into_iter().map(|(role, text)|
        (role, duduclaw_security::credential_detect::mask(&text))).collect();
    let mut rolling = Vec::new();
    if !checked.summary.is_empty() {
        rolling.push(("background".to_owned(), duduclaw_security::credential_detect::mask(checked.summary)));
    }
    rolling.extend(turns);
    let (transcript, protected, has_unprotected_text) = partition_turns_for_summary(&rolling);
    let transcript = crate::session_summarizer::rolling_summary_input("", &transcript);
    if transcript.trim().is_empty() {
        return Err(SummaryFailure::EmptyInput);
    }

    let agent_id = session_manager.session_agent(session_id).await
        .map_err(|_| SummaryFailure::ReadState)?;
    let memory_language = crate::prompt_identity::resolve_memory_language_for_id(home_dir, agent_id.as_deref().unwrap_or("")).await;
    let summary = if has_unprotected_text {
        let prompt = crate::session_summarizer::format_summarization_prompt_with_language(&transcript, memory_language.as_deref());
        let agent_dir = crate::runtime_config::session_agent_dir(home_dir, agent_id.as_deref())
            .map_err(|_| SummaryFailure::ReadState)?;
        let agent_id = agent_dir.as_ref().and(agent_id.as_deref()).unwrap_or("");
        // The protected sections never enter the utility-model prompt.
        crate::runtime_dispatch::run_utility_prompt(
            home_dir,
            agent_dir.as_deref(),
            agent_id,
            "",
            &prompt,
            crate::runtime_dispatch::UTILITY_MAX_TOKENS,
        )
        .await
        .map_err(|_| SummaryFailure::Utility)?
    } else {
        String::new()
    };

    let summary = crate::prompt_identity::align_memory_script(&summary, memory_language.as_deref(), &transcript);
    let trimmed = if has_unprotected_text {
        crate::session_summarizer::validate_summary_output(&summary)?
    } else { "" };
    let protected = cap_protected_for_summary(&protected);
    let persisted = if protected.is_empty() {
        trimmed.to_string()
    } else if trimmed.is_empty() {
        format!("[verbatim protected sections]\n{protected}")
    } else {
        format!("{trimmed}\n[verbatim protected sections]\n{protected}")
    };
    let bytes = persisted.len();
    session_manager
        .set_summary_if_unchanged(session_id, &persisted, through_turn, after, &expected_ids)
        .await
        .map_err(|_| SummaryFailure::Store)?;
    Ok(Some(bytes))
}

/// Hard byte cap on the verbatim protected text this task pins into a session
/// summary.
///
/// Review finding 4 (third leg): `set_summary` has no length limit and the
/// summary is injected into the system prompt on **every** subsequent turn. A
/// never-trim header is plain markdown any channel user can type, so one
/// message with `## Constraints` plus a wall of text was pinned verbatim,
/// forever, at a cost paid per turn — and unlike the budget-floor path this
/// amplification is independent of whether a budget is configured at all.
/// 4 KiB is comfortably above what a real packet's `constraints` + `audience`
/// occupy.
pub const PROTECTED_SUMMARY_MAX_BYTES: usize = 4 * 1024;

/// Apply [`PROTECTED_SUMMARY_MAX_BYTES`], leaving a trace in the text itself.
///
/// Truncation is CJK-safe ([`duduclaw_core::truncate_bytes`], coding
/// convention 1) and the marker is part of the persisted string rather than a
/// log line only: a reader of the summary must be able to tell that it is
/// holding a prefix, otherwise the pinned constraint silently becomes a
/// different constraint.
pub(crate) fn cap_protected_for_summary(protected: &str) -> String {
    if protected.len() <= PROTECTED_SUMMARY_MAX_BYTES {
        return protected.to_string();
    }
    let kept = duduclaw_core::truncate_bytes(protected, PROTECTED_SUMMARY_MAX_BYTES);
    tracing::warn!(
        protected_bytes = protected.len(),
        cap = PROTECTED_SUMMARY_MAX_BYTES,
        "session summary: verbatim protected sections exceed the cap — the excess is \
         treated as ordinary compressible content and is NOT pinned into the summary"
    );
    format!(
        "{kept}\n[protected sections truncated at {PROTECTED_SUMMARY_MAX_BYTES} bytes \
         ({} bytes were offered); the remainder is ordinary compressible history]",
        protected.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct SessionUtilityRuntime;

    #[async_trait::async_trait]
    impl crate::runtime::AgentRuntime for SessionUtilityRuntime {
        fn name(&self) -> &str { "openai_compat" }
        async fn is_available(&self) -> bool { panic!("owned registry must not probe host") }
        async fn execute(&self, prompt: &str, context: &crate::runtime::RuntimeContext)
            -> Result<crate::runtime::RuntimeResponse, String> {
            if context.home_dir.join("summary-clamp-fixture").exists() {
                assert!(!prompt.contains("stale summary must be ignored"));
                assert!(prompt.contains("user: raw fact 0"));
                assert!(prompt.contains("user: raw fact 58"));
                assert!(!prompt.contains("user: raw fact 59"));
            }
            if context.home_dir.join("summary-output-fixture").exists() {
                assert!(prompt.contains("- Previously established fact"));
                assert!(prompt.contains("new 中文"));
                assert!(!prompt.contains("old one"));
                assert!(!prompt.contains("old two"));
                assert!(!prompt.contains("user: latest"));
            }
            assert!(prompt.len() <= crate::session_summarizer::SUMMARY_INPUT_MAX_BYTES);
            assert_eq!(context.agent_id, "utility-worker");
            assert_eq!(context.model, "gpt-5.4");
            assert_eq!(context.agent_dir.as_deref(), Some(context.home_dir.join("agents/utility-worker").canonicalize().unwrap().as_path()));
            Ok(crate::runtime::RuntimeResponse {
                partial_timeout_secs: None,
                content: std::fs::read_to_string(context.home_dir.join("summary-output-fixture"))
                    .unwrap_or_else(|_| "Session utility result".into()), input_tokens: 1, output_tokens: 1,
                cache_read_tokens: 0, model_used: context.model.clone(), runtime_name: "openai_compat".into(),
            })
        }
    }

    #[tokio::test]
    async fn stale_target_skips_below_trigger_without_writing() {
        let (sm, home) = make_session_manager();
        sm.get_or_create("shrunk", "worker").await.unwrap();
        for _ in 0..20 { sm.append_message("shrunk", "user", "fact", 1).await.unwrap(); }
        let ids = sm.get_message_ids("shrunk").await.unwrap();
        for id in &ids[..15] { sm.hide_message("shrunk", *id).await.unwrap(); }
        let conn = rusqlite::Connection::open(home.path().join("sessions.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_summary_write BEFORE UPDATE OF summary_of_prior
            ON sessions BEGIN SELECT RAISE(ABORT, 'unexpected summary write'); END;").unwrap();
        assert_eq!(summarize_one(&sm, home.path(), "shrunk", 17, 10).await.unwrap(), None);
        assert_eq!(sm.get_summary("shrunk").await.unwrap(), (String::new(), 0));
        conn.execute_batch("DROP TRIGGER reject_summary_write;").unwrap();
        sm.set_summary("shrunk", "- current", 2).await.unwrap();
        assert_eq!(summarize_one(&sm, home.path(), "shrunk", 17, 1).await.unwrap(), None);
        assert_eq!(sm.get_summary("shrunk").await.unwrap(), ("- current".into(), 2));
    }

    #[tokio::test]
    async fn stale_target_is_capped_to_current_raw_tail() {
        let (sm, home) = make_session_manager();
        sm.get_or_create("capped", "worker").await.unwrap();
        let text = format!("## 約束\n{}\n- retain", marker());
        for _ in 0..5 { sm.append_message("capped", "user", &text, 1).await.unwrap(); }
        let bytes = summarize_one(&sm, home.path(), "capped", 17, 1).await.unwrap();
        assert!(bytes.is_some());
        assert_eq!(sm.get_summary("capped").await.unwrap().1, 2);
        assert_eq!(sm.get_messages("capped").await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn clamped_summary_reads_raw_prefix_and_replaces_original_cursor() {
        let (sm, home) = make_session_manager();
        let dir = home.path().join("agents/utility-worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname='utility-worker'\nstatus='active'\n[runtime]\nprovider='openai_compat'\n[model]\npreferred='gpt-5.4'\n").unwrap();
        std::fs::write(home.path().join("summary-clamp-fixture"), "").unwrap();
        let runtimes = std::collections::HashMap::from([(
            crate::runtime::RuntimeType::OpenAiCompat,
            Box::new(SessionUtilityRuntime) as Box<dyn crate::runtime::AgentRuntime>,
        )]);
        let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
            home.path(), crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
        ).await;
        sm.get_or_create("stuck", "utility-worker").await.unwrap();
        for i in 0..62 {
            sm.append_message("stuck", "user", &format!("raw fact {i}"), 1).await.unwrap();
        }
        rusqlite::Connection::open(home.path().join("sessions.db")).unwrap().execute(
            "UPDATE sessions SET summary_of_prior='- stale summary must be ignored',
             summarized_through_turn=260 WHERE id='stuck'", [],
        ).unwrap();
        let ids = sm.get_message_ids("stuck").await.unwrap();
        crate::autonomous_runtime::scope(home.path(), "utility-worker", &[],
            summarize_one(&sm, home.path(), "stuck", 59, 1)).await.unwrap();
        assert_eq!(sm.get_summary("stuck").await.unwrap(), ("Session utility result".into(), 59));
        assert_eq!(sm.get_message_ids("stuck").await.unwrap(), ids);
    }

    #[tokio::test]
    async fn refusal_keeps_previous_summary_cursor_and_history() {
        let (sm, home) = make_session_manager();
        let dir = home.path().join("agents/utility-worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname='utility-worker'\nstatus='active'\n[runtime]\nprovider='openai_compat'\n[model]\npreferred='gpt-5.4'\n").unwrap();
        std::fs::write(home.path().join("summary-output-fixture"), "I cannot summarize this conversation.").unwrap();
        let runtimes = std::collections::HashMap::from([(
            crate::runtime::RuntimeType::OpenAiCompat,
            Box::new(SessionUtilityRuntime) as Box<dyn crate::runtime::AgentRuntime>,
        )]);
        let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
            home.path(), crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
        ).await;
        for sid in ["owned", "odoolivechat:visitor"] {
            sm.get_or_create(sid, "utility-worker").await.unwrap();
            for text in ["old one", "old two", "new 中文", "latest", "raw tail", "current"] {
                sm.append_message(sid, "user", text, 1).await.unwrap();
            }
            sm.set_summary(sid, "- Previously established fact", 2).await.unwrap();
            let ids = sm.get_message_ids(sid).await.unwrap();
            let result = crate::autonomous_runtime::scope(home.path(), "utility-worker", &[],
                summarize_one(&sm, home.path(), sid, 3, 1)).await;
            assert_eq!(result, Err(crate::session_summarizer::SummaryFailure::RefusalOrMeta));
            assert_eq!(sm.get_summary(sid).await.unwrap(), ("- Previously established fact".into(), 2));
            assert_eq!(sm.get_message_ids(sid).await.unwrap(), ids);
            let delta = sm.read_summary_turn_range(sid, 2, 3).await.unwrap();
            assert_eq!(delta, vec![("user".into(), "new 中文".into())]);
        }
    }

    #[tokio::test]
    async fn failed_tick_backs_off_without_changing_history_even_after_restart() {
        let (sm, home) = make_session_manager();
        sm.get_or_create("bad-owner", "../missing").await.unwrap();
        for _ in 0..12 { sm.append_message("bad-owner", "user", "keep this", 1).await.unwrap(); }
        let before = sm.get_message_ids("bad-owner").await.unwrap();
        tick_once(&sm, home.path(), &SummarizeParams::default()).await;
        assert_eq!(sm.get_message_ids("bad-owner").await.unwrap(), before);
        assert_eq!(sm.get_summary("bad-owner").await.unwrap(), (String::new(), 0));
        let path = home.path().join("session_summary_retry.json");
        let first = std::fs::read(&path).unwrap();
        let restarted = SessionManager::new(&home.path().join("sessions.db")).unwrap();
        tick_once(&restarted, home.path(), &SummarizeParams::default()).await;
        assert_eq!(std::fs::read(&path).unwrap(), first);
        assert_eq!(restarted.get_message_ids("bad-owner").await.unwrap(), before);
    }

    #[tokio::test]
    async fn owned_session_dispatches_through_agent_runtime() {
        let (sm, home) = make_session_manager();
        let dir = home.path().join("agents/utility-worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname='utility-worker'\nstatus='active'\n[runtime]\nprovider='openai_compat'\n[model]\npreferred='gpt-5.4'\n").unwrap();
        // A mistaken global dispatch also reaches the fake runtime, but with
        // a different model/id, failing before any external call can occur.
        std::fs::write(home.path().join("config.toml"), "[runtime]\nutility_provider='openai_compat'\nutility_model='gpt-5.4-mini'\n").unwrap();
        let runtimes = std::collections::HashMap::from([(
            crate::runtime::RuntimeType::OpenAiCompat,
            Box::new(SessionUtilityRuntime) as Box<dyn crate::runtime::AgentRuntime>,
        )]);
        let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
            home.path(), crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
        ).await;
        sm.get_or_create("owned", "utility-worker").await.unwrap();
        sm.append_message("owned", "user", "Summarize this session", 4).await.unwrap();
        sm.append_message("owned", "assistant", "Here is the discussion", 4).await.unwrap();
        for _ in 0..3 { sm.append_message("owned", "user", "raw tail", 1).await.unwrap(); }
        // Production `tick_once` scopes the owner before `summarize_one`.
        crate::autonomous_runtime::scope(
            home.path(), "utility-worker", &[],
            summarize_one(&sm, home.path(), "owned", 2, 1),
        ).await.unwrap();
        assert_eq!(sm.get_summary("owned").await.unwrap(), ("Session utility result".into(), 2));
    }

    #[tokio::test]
    async fn refused_owner_runtime_uses_tool_free_global_spec() {
        let (sm, home) = make_session_manager();
        let dir = home.path().join("agents/utility-worker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[agent]\nname='utility-worker'\nstatus='active'\n[runtime]\nprovider='grok'\n[model]\npreferred='grok-3'\n").unwrap();
        std::fs::write(home.path().join("config.toml"), "[runtime]\nutility_provider='openai_compat'\nutility_model='gpt-5.4'\n").unwrap();
        let runtimes = std::collections::HashMap::from([(
            crate::runtime::RuntimeType::OpenAiCompat,
            Box::new(SessionUtilityRuntime) as Box<dyn crate::runtime::AgentRuntime>,
        )]);
        let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
            home.path(), crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
        ).await;
        sm.get_or_create("fallback-owner", "utility-worker").await.unwrap();
        sm.append_message("fallback-owner", "user", "Summarize this session", 4).await.unwrap();
        sm.append_message("fallback-owner", "assistant", "Here is the discussion", 4).await.unwrap();
        for _ in 0..3 { sm.append_message("fallback-owner", "user", "raw tail", 1).await.unwrap(); }
        crate::autonomous_runtime::scope(
            home.path(), "utility-worker", &[],
            summarize_one(&sm, home.path(), "fallback-owner", 2, 1),
        ).await.unwrap();
        assert_eq!(sm.get_summary("fallback-owner").await.unwrap().0, "Session utility result");
    }

    #[tokio::test]
    async fn invalid_stored_owner_never_reaches_global_utility() {
        let (sm, home) = make_session_manager();
        // If fallback dispatch occurs this fake global runtime panics because
        // it only accepts the named employee and model used by the valid test.
        std::fs::write(home.path().join("config.toml"), "[runtime]\nutility_provider='openai_compat'\nutility_model='gpt-5.4-mini'\n").unwrap();
        let runtimes = std::collections::HashMap::from([(
            crate::runtime::RuntimeType::OpenAiCompat,
            Box::new(SessionUtilityRuntime) as Box<dyn crate::runtime::AgentRuntime>,
        )]);
        let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
            home.path(), crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
        ).await;
        for (index, owner) in ["missing", "../worker", "dashboard", " "].into_iter().enumerate() {
            let sid = format!("invalid-owner-{index}");
            sm.get_or_create(&sid, owner).await.unwrap();
            sm.append_message(&sid, "user", "Summarize this conversation", 4).await.unwrap();
            sm.append_message(&sid, "assistant", "Discussion content", 4).await.unwrap();
            for _ in 0..3 { sm.append_message(&sid, "user", "raw tail", 1).await.unwrap(); }
            let error = summarize_one(&sm, home.path(), &sid, 2, 1).await.unwrap_err();
            assert_eq!(error, crate::session_summarizer::SummaryFailure::ReadState);
            assert_eq!(sm.get_summary(&sid).await.unwrap().0, "");
        }
    }

    #[tokio::test]
    async fn session_utility_uses_valid_existing_agent_only() {
        let (sm, home) = make_session_manager();
        let agent_dir = home.path().join("agents/utility-worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"),
            "[agent]\nname = \"utility-worker\"\n[runtime]\nprovider = \"openai_compat\"\n[model]\npreferred = \"gpt-5.4\"\n").unwrap();
        std::fs::create_dir_all(home.path().join("utility-worker")).unwrap();
        std::fs::write(home.path().join("agents/file-agent"), "not a directory").unwrap();
        for (sid, owner) in [("file", "file-agent"), ("known", "utility-worker"), ("unknown", "missing"),
            ("invalid", "../utility-worker"), ("absolute", "/tmp"), ("blank", "")] {
            sm.get_or_create(sid, owner).await.unwrap();
            let id = sm.session_agent(sid).await.unwrap();
            let dir = crate::runtime_config::session_agent_dir(home.path(), id.as_deref());
            if sid == "known" {
                let dir = dir.unwrap();
                assert_eq!(dir.as_deref(), Some(agent_dir.canonicalize().unwrap().as_path()));
                let spec = crate::runtime_config::resolve_utility(home.path(), dir.as_deref());
                assert_eq!(spec.provider, duduclaw_core::types::RuntimeType::OpenAiCompat);
                assert_eq!(spec.model, "gpt-5.4");
            } else if sid == "blank" {
                assert!(dir.unwrap().is_none());
            } else {
                assert!(dir.is_err(), "{sid}");
            }
        }
        assert!(crate::runtime_config::session_agent_dir(home.path(), None).unwrap().is_none());
    }

    /// Build a SessionManager backed by a temp DB so tests don't
    /// pollute the real `~/.duduclaw/sessions.db`.
    fn make_session_manager() -> (Arc<SessionManager>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions.db");
        let sm = Arc::new(SessionManager::new(&db_path).unwrap());
        (sm, tmp)
    }

    /// This process's protected marker — see
    /// [`duduclaw_core::protected_section`]. A fixture without it is exactly
    /// what a channel user can type.
    fn marker() -> String {
        duduclaw_core::protected_section::protected_marker_line(
            duduclaw_core::protected_section::process_sentinel(),
        )
    }

    #[test]
    fn protected_sections_stay_out_of_utility_prompt_and_keep_turn_boundaries() {
        let m = marker();
        let turns = vec![
            (
                "user".into(),
                format!("Question before\n## 約束\n{m}\n- secret constraint\n"),
            ),
            (
                "assistant".into(),
                format!("A separate reply\n## 受眾\n{m}\n- private audience\n"),
            ),
        ];
        let (transcript, protected, has_visible) = partition_turns_for_summary(&turns);
        assert!(has_visible);
        assert!(transcript.contains("user: Question before"));
        assert!(transcript.contains("assistant: A separate reply"));
        let prompt = crate::session_summarizer::format_summarization_prompt(&transcript);
        assert!(!prompt.contains("secret constraint"));
        assert!(!prompt.contains("private audience"));
        assert!(protected.contains(&format!("## 約束\n{m}\n- secret constraint\n")));
        assert!(protected.contains(&format!("## 受眾\n{m}\n- private audience\n")));
        let plain = vec![
            ("user".into(), "hello".into()),
            ("assistant".into(), "hi".into()),
        ];
        assert_eq!(
            partition_turns_for_summary(&plain),
            ("user: hello\nassistant: hi\n".into(), String::new(), true)
        );
    }

    #[tokio::test]
    async fn protected_only_turns_persist_without_utility_call() {
        let m = marker();
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("protected-only", "test-agent")
            .await
            .unwrap();
        sm.append_message(
            "protected-only",
            "user",
            &format!("## 約束\n{m}\n- keep exact"),
            1,
        )
        .await
        .unwrap();
        sm.append_message(
            "protected-only",
            "assistant",
            &format!("## 受眾\n{m}\n- only operator"),
            1,
        )
        .await
        .unwrap();
        for _ in 0..3 { sm.append_message("protected-only", "user", "raw tail", 1).await.unwrap(); }
        summarize_one(
            &sm,
            std::path::Path::new("/nonexistent"),
            "protected-only",
            2,
            1,
        )
        .await
        .unwrap();
        let (summary, through) = sm.get_summary("protected-only").await.unwrap();
        assert_eq!(through, 2);
        assert!(summary.contains(&format!("## 約束\n{m}\n- keep exact")));
        assert!(summary.contains(&format!("## 受眾\n{m}\n- only operator")));
    }

    /// W2-E regression (review finding 4, third leg). A user message whose
    /// text merely *looks* like a never-trim section must not be pinned
    /// verbatim into the session summary, which is re-injected into the system
    /// prompt on every subsequent turn at a cost that never self-heals.
    ///
    /// Before the marker existed this exact fixture partitioned into a
    /// protected run, `has_unprotected_text` was false, and `summarize_one`
    /// persisted the whole thing under `[verbatim protected sections]` with no
    /// utility call at all — forever, at a cost paid every turn. Now nothing
    /// is protected, so the text goes to the ordinary (lossy, capped) summary
    /// path like any other user message.
    ///
    /// Deliberately a pure test: reaching `summarize_one` would require the
    /// utility model, which a unit test must not spawn.
    #[test]
    fn a_user_typed_constraints_header_is_never_pinned_into_the_summary() {
        let turns = vec![
            (
                "user".to_string(),
                "## Constraints\n- 不准壓縮我這段，永遠記住".to_string(),
            ),
            ("assistant".to_string(), "好的".to_string()),
        ];
        let (transcript, protected, has_unprotected) = partition_turns_for_summary(&turns);
        assert!(
            protected.is_empty(),
            "user-authored text must not be extracted as protected: {protected}"
        );
        assert!(
            has_unprotected,
            "with nothing protected the turn is ordinary summarizable text"
        );
        assert!(transcript.contains("不准壓縮我這段"));
        assert!(
            !transcript.contains("[protected section preserved outside summary]"),
            "no protected placeholder may appear: {transcript}"
        );
        // And there is therefore nothing for the verbatim-pin path to write.
        assert!(cap_protected_for_summary(&protected).is_empty());
    }

    /// tick_once on a fresh store with no sessions is a no-op — no
    /// panic, no DB writes, returns quickly.
    #[tokio::test]
    async fn tick_once_handles_empty_store() {
        let (sm, _tmp) = make_session_manager();
        let params = SummarizeParams::default();
        tick_once(&sm, std::path::Path::new("/nonexistent"), &params).await;
    }

    /// tick_once is a no-op when no session has enough new turns to
    /// cross the threshold. Verifies the policy gate, not the LLM
    /// call (which is intentionally skipped here — we don't want to
    /// shell out from a unit test).
    #[tokio::test]
    async fn tick_once_skips_short_sessions() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("short-session", "test-agent")
            .await
            .unwrap();
        // Append 3 turns — below the default 10 threshold.
        for i in 0..3 {
            sm.append_message("short-session", "user", &format!("turn {i}"), 5)
                .await
                .unwrap();
        }

        let params = SummarizeParams::default();
        tick_once(&sm, std::path::Path::new("/nonexistent"), &params).await;

        // Summary must still be empty — no Haiku call was triggered.
        let (summary, through) = sm.get_summary("short-session").await.unwrap();
        assert!(summary.is_empty());
        assert_eq!(through, 0);
    }

    /// Confirm that `list_summary_candidates` reports the expected shape:
    /// sessions with their turn count, prior summarized turn, and
    /// seconds-since-last-summary (None when never summarized).
    #[tokio::test]
    async fn list_summary_candidates_reflects_store() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("s1", "agent-a").await.unwrap();
        for _ in 0..15 {
            sm.append_message("s1", "user", "hi", 2).await.unwrap();
        }
        let c = sm.list_summary_candidates().await.unwrap();
        let row = c.iter().find(|c| c.session_id == "s1").unwrap();
        assert_eq!(row.turn_count, 15);
        assert_eq!(row.summarized_through_turn, 0);
        assert!(row.seconds_since_last_summary.is_none());
    }

    /// After `set_summary`, the candidate row should reflect the
    /// summarized turn count and a recent `seconds_since_last_summary`.
    #[tokio::test]
    async fn set_summary_updates_candidate_row() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("s2", "agent-a").await.unwrap();
        for _ in 0..20 {
            sm.append_message("s2", "user", "hi", 2).await.unwrap();
        }
        sm.set_summary("s2", "- bullet one\n- bullet two", 15)
            .await
            .unwrap();

        let (summary, through) = sm.get_summary("s2").await.unwrap();
        assert!(summary.contains("bullet one"));
        assert_eq!(through, 15);

        let c = sm.list_summary_candidates().await.unwrap();
        let row = c.iter().find(|c| c.session_id == "s2").unwrap();
        assert_eq!(row.summarized_through_turn, 15);
        // A non-zero, small "seconds since" — we just wrote it.
        let secs = row
            .seconds_since_last_summary
            .expect("must have last_summarized_at after set_summary");
        assert!(secs < 60, "expected recent summary, got {secs}s");
    }

    /// `read_first_n_turns_text` returns turns in insertion order with
    /// "role: content" lines. Used by the summarizer to build the
    /// transcript fed to Haiku.
    #[tokio::test]
    async fn read_first_n_returns_role_prefixed_lines() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("s3", "agent-a").await.unwrap();
        sm.append_message("s3", "user", "hello", 1).await.unwrap();
        sm.append_message("s3", "assistant", "hi there", 2)
            .await
            .unwrap();
        sm.append_message("s3", "user", "another", 1).await.unwrap();

        let text = sm.read_first_n_turns_text("s3", 2).await.unwrap();
        assert!(text.contains("user: hello"));
        assert!(text.contains("assistant: hi there"));
        // Third turn must NOT be included (we asked for first 2).
        assert!(!text.contains("another"));
    }

    /// Review finding 4 (third leg) regression: whatever
    /// `partition_turns_for_summary` classifies as protected used to be pinned
    /// into `set_summary` verbatim with **no** length limit, and the summary is
    /// injected into the system prompt on every later turn. A never-trim
    /// heading is plain markdown any channel user can send, so one message was
    /// enough to pin an unbounded wall of text forever.
    #[test]
    fn a_huge_protected_section_is_capped_before_it_is_pinned_into_a_summary() {
        let small = format!("## 約束\n- {}\n", "短".repeat(10));
        assert_eq!(
            cap_protected_for_summary(&small),
            small,
            "content under the cap must pass through byte-identical"
        );

        let huge = format!("## 約束\n- {}\n", "長".repeat(PROTECTED_SUMMARY_MAX_BYTES));
        let capped = cap_protected_for_summary(&huge);
        assert!(
            capped.len() < huge.len(),
            "over-cap content must be trimmed: {} vs {}",
            capped.len(),
            huge.len()
        );
        assert!(
            capped.contains("protected sections truncated"),
            "the truncation must be stated in the persisted text, not only logged: {}",
            duduclaw_core::truncate_chars(&capped, 200)
        );
        // CJK-safe: the kept prefix is still valid UTF-8 at a char boundary
        // (coding convention 1 — `truncate_bytes`, never a raw byte slice).
        assert!(capped.chars().count() > 0);
        // The marker adds a bounded suffix; the kept payload respects the cap.
        let marker_at = capped.find("\n[protected sections truncated").unwrap();
        assert!(marker_at <= PROTECTED_SUMMARY_MAX_BYTES);
    }
}
