//! RM-206 (v1.84.0): conversation continuity at the engine boundary.
//!
//! These run the real channel pipeline (`build_reply_for_agent`: session
//! store, prompt build, dispatch, failover) and observe what each engine's
//! `AgentRuntime::execute` receives through `RegistryGuard` recording
//! runtimes. What a runtime then hands its process is pinned next to each
//! runtime (Codex: `runtime/codex.rs` `capture_conversation`; Claude:
//! `runtime/claude.rs` `HISTORY_CAPTURE`; Grok / generic CLIs:
//! `runtime/prompt_budget.rs`). The recording "Grok" here also builds the
//! real single-argument payload, so the section layout handed over by
//! `inner.rs` is exercised end to end. No network, no vendor CLI.

use crate::runtime::{AgentRuntime, RuntimeContext, RuntimeResponse, RuntimeType};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct Seen {
    prompt: String,
    system_prompt: String,
    history: Vec<(String, String)>,
    /// Grok / generic CLI: the real single-argument payload.
    payload: Option<String>,
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<(RuntimeType, Seen)>>>);

impl Log {
    fn of(&self, runtime: RuntimeType) -> Vec<Seen> {
        self.0.lock().unwrap().iter().filter(|(rt, _)| *rt == runtime).map(|(_, s)| s.clone()).collect()
    }
}

struct Recorder {
    id: RuntimeType,
    log: Log,
    fail: bool,
}

#[async_trait::async_trait]
impl AgentRuntime for Recorder {
    fn name(&self) -> &str { self.id.as_str() }
    async fn is_available(&self) -> bool { true }
    async fn execute(&self, prompt: &str, context: &RuntimeContext) -> Result<RuntimeResponse, String> {
        // Background work in the same process is not a conversation turn.
        if crate::runtime_dispatch::in_utility_call() {
            return Err("utility disabled in continuity fixture".into());
        }
        let payload = matches!(self.id, RuntimeType::Grok | RuntimeType::Qwen)
            .then(|| crate::runtime::generic_cli::build_prompt(context, prompt, self.id.as_str()));
        self.log.0.lock().unwrap().push((self.id, Seen {
            prompt: prompt.to_string(),
            system_prompt: context.system_prompt.clone(),
            history: context.conversation_history.iter().map(|t| (t.role.clone(), t.content.clone())).collect(),
            payload,
        }));
        if self.fail {
            return Err("500 internal server error".into());
        }
        let turn = self.log.of(self.id).len();
        Ok(RuntimeResponse {
            content: format!("REPLY-{turn}-FROM-{}", self.id.as_str()),
            model_used: context.model.clone(),
            runtime_name: self.id.as_str().into(),
            ..Default::default()
        })
    }
}

fn preferred_model(provider: RuntimeType) -> String {
    match provider {
        RuntimeType::OpenAiCompat => "gpt-5.4".into(),
        other => other.spec().fallback_models.first().map(|m| m.0.to_string()).unwrap_or_default(),
    }
}

struct Fixture {
    _home: tempfile::TempDir,
    ctx: crate::channel_reply::ReplyContext,
    log: Log,
    _registry: crate::runtime_dispatch::testing::RegistryGuard,
}

/// One employee `worker` on `primary` (optionally failing, with `fallback`).
async fn fixture(primary: RuntimeType, fallback: Option<RuntimeType>, primary_fails: bool, soul: Option<&str>) -> Fixture {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),
        "[general]\ninference_mode='claude'\n[goal_intent]\nenabled=false\nmode='off'\n").unwrap();
    let dir = home.path().join("agents/worker");
    std::fs::create_dir_all(&dir).unwrap();
    let fallback_line = fallback.map(|f| format!("fallback='{}'\n", f.as_str())).unwrap_or_default();
    std::fs::write(dir.join("agent.toml"), format!(
        "[agent]\nname='worker'\ndisplay_name='Actual Worker'\nrole='specialist'\nstatus='active'\ntrigger='@Worker'\nreports_to=''\nicon='🤖'\n\
         [model]\npreferred='{}'\nfallback=''\naccount_pool=[]\n[runtime]\nprovider='{}'\n{fallback_line}\
         [heartbeat]\nenabled=false\ninterval_seconds=3600\nmax_concurrent_runs=1\ncron=''\n\
         [permissions]\nallowed_channels=['*']\n[goal_intent]\nenabled=false\nmode='off'\n[evolution]\nenabled=false\n",
        preferred_model(primary), primary.as_str(),
    )).unwrap();
    if let Some(soul) = soul {
        std::fs::write(dir.join("SOUL.md"), soul).unwrap();
    }
    let registry = Arc::new(tokio::sync::RwLock::new(duduclaw_agent::AgentRegistry::new(home.path().join("agents"))));
    registry.write().await.scan().await.unwrap();
    let sessions = Arc::new(crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap());
    let status = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let ctx = crate::channel_reply::ReplyContext::new(registry, home.path().to_owned(), sessions, status, tx);
    let log = Log::default();
    let mut runtimes: std::collections::HashMap<RuntimeType, Box<dyn AgentRuntime>> = std::collections::HashMap::new();
    runtimes.insert(primary, Box::new(Recorder { id: primary, log: log.clone(), fail: primary_fails }));
    if let Some(fallback) = fallback {
        runtimes.insert(fallback, Box::new(Recorder { id: fallback, log: log.clone(), fail: false }));
    }
    let _registry = crate::runtime_dispatch::testing::RegistryGuard::install(
        home.path(), crate::runtime::RuntimeRegistry::with_runtimes(runtimes)).await;
    Fixture { _home: home, ctx, log, _registry }
}

async fn turn(f: &Fixture, sid: &str, text: &str) -> String {
    crate::channel_reply::build_reply_for_agent(text, &f.ctx, "worker", sid, "continuity-user", None).await
}

/// Seed a compacted conversation: 8 rows, the first 4 summarised by the
/// rolling summary, plus pinned instructions.
async fn seed_compacted(f: &Fixture, sid: &str) {
    let sm = &f.ctx.session_manager;
    sm.get_or_create(sid, "worker").await.unwrap();
    for k in 1..=8 {
        let role = if k % 2 == 1 { "user" } else { "assistant" };
        sm.append_message(sid, role, &format!("OLD-ROW-{k}"), 1).await.unwrap();
    }
    sm.set_summary(sid, "- ROLLING-SUMMARY-MARKER the chosen colour is blue", 4).await.unwrap();
    sm.set_pinned(sid, "PINNED-MARKER always answer in Traditional Chinese").await.unwrap();
}

fn has(history: &[(String, String)], role: &str, needle: &str) -> bool {
    history.iter().any(|(r, c)| r == role && c.contains(needle))
}

const ENGINES: [RuntimeType; 4] = [RuntimeType::Codex, RuntimeType::Grok, RuntimeType::Qwen, RuntimeType::OpenAiCompat];

/// (1) Turn 2 carries turn 1's user text and reply, for every registry engine.
#[tokio::test(flavor = "current_thread")]
async fn second_turn_carries_the_first_turn_on_every_engine() {
    for engine in ENGINES {
        let f = fixture(engine, None, false, None).await;
        let sid = "discord:continuity-two-turns";
        let first = turn(&f, sid, "TURN-ONE-USER 我的代號是藍鯨").await;
        assert!(first.contains("REPLY-1-FROM"), "{engine:?}: {first}");
        turn(&f, sid, "TURN-TWO-USER 代號是什麼？").await;
        let seen = f.log.of(engine);
        assert_eq!(seen.len(), 2, "{engine:?}");
        assert!(seen[0].history.iter().all(|(_, c)| !c.contains("TURN-ONE-USER")), "{engine:?}: turn 1 is the prompt, not history");
        assert!(has(&seen[1].history, "user", "TURN-ONE-USER"), "{engine:?}: {:?}", seen[1].history);
        assert!(has(&seen[1].history, "assistant", "REPLY-1-FROM"), "{engine:?}: {:?}", seen[1].history);
        assert!(seen[1].prompt.contains("TURN-TWO-USER"), "{engine:?}");
        if let Some(payload) = &seen[1].payload {
            assert!(payload.contains("TURN-ONE-USER") && payload.contains("REPLY-1-FROM"), "{engine:?}: argv payload");
        }
    }
}

/// (2) After compaction the engine receives the summary AND the most recent
/// verbatim rows (at least three), plus the pinned instructions.
#[tokio::test(flavor = "current_thread")]
async fn compacted_conversation_reaches_the_engine_with_summary_and_recent_turns() {
    for engine in ENGINES {
        let f = fixture(engine, None, false, None).await;
        let sid = "discord:continuity-compacted";
        seed_compacted(&f, sid).await;
        turn(&f, sid, "TURN-NOW").await;
        let seen = f.log.of(engine);
        assert_eq!(seen.len(), 1, "{engine:?}");
        let seen = &seen[0];
        assert!(seen.system_prompt.contains("ROLLING-SUMMARY-MARKER"), "{engine:?}");
        assert!(seen.system_prompt.contains("PINNED-MARKER"), "{engine:?}");
        for k in 6..=8 {
            let role = if k % 2 == 1 { "user" } else { "assistant" };
            assert!(has(&seen.history, role, &format!("OLD-ROW-{k}")), "{engine:?}: row {k} missing: {:?}", seen.history);
        }
        assert!(!has(&seen.history, "user", "OLD-ROW-1"), "{engine:?}: summarised rows are not replayed");
        assert!(seen.prompt.contains("<task_recap>") && seen.prompt.contains("PINNED-MARKER"), "{engine:?}");
        if let Some(payload) = &seen.payload {
            assert!(payload.contains("ROLLING-SUMMARY-MARKER") && payload.contains("PINNED-MARKER") && payload.contains("OLD-ROW-8"));
        }
    }
}

/// (3) After a runtime fallback the fallback engine receives the same
/// history and the same summary / pinned blocks as the primary.
#[tokio::test(flavor = "current_thread")]
async fn fallback_engine_receives_the_same_conversation_as_the_primary() {
    for (primary, fallback) in [
        (RuntimeType::Codex, RuntimeType::Claude),
        (RuntimeType::Grok, RuntimeType::Claude),
        (RuntimeType::Codex, RuntimeType::Grok),
    ] {
        let f = fixture(primary, Some(fallback), true, None).await;
        let sid = "discord:continuity-fallback";
        seed_compacted(&f, sid).await;
        let reply = turn(&f, sid, "TURN-NOW").await;
        assert!(reply.contains(&format!("FROM-{}", fallback.as_str())), "{primary:?}→{fallback:?}: {reply}");
        let (p, b) = (f.log.of(primary), f.log.of(fallback));
        assert_eq!((p.len(), b.len()), (1, 1), "{primary:?}→{fallback:?}");
        assert_eq!(b[0].history, p[0].history, "{primary:?}→{fallback:?}: same history");
        assert_eq!(b[0].system_prompt, p[0].system_prompt, "{primary:?}→{fallback:?}: same summary/pinned blocks");
        assert_eq!(b[0].prompt, p[0].prompt, "{primary:?}→{fallback:?}: same task recap and message");
        assert!(b[0].system_prompt.contains("ROLLING-SUMMARY-MARKER") && b[0].system_prompt.contains("PINNED-MARKER"));
        assert!(has(&b[0].history, "assistant", "OLD-ROW-8"));
    }
}

/// (4) A > 64 KB system prompt: the summary and pinned instructions still
/// reach Grok inside the single-argument budget, built from the section
/// layout `inner.rs` hands over — with the recent rows while the base prompt
/// fits beside them, and without them once the base prompt itself has to be
/// cut (security review: conversation history is dropped before any base
/// prompt text, so a cut base prompt never sits next to it).
#[tokio::test(flavor = "current_thread")]
async fn oversized_prompt_keeps_summary_and_pinned_in_the_grok_argument() {
    for (repeat, rows_kept) in [(2_000, true), (4_500, false)] {
        let soul = format!("# Soul\n{}", "這是一段很長的員工設定。".repeat(repeat));
        if !rows_kept {
            assert!(soul.len() > 128 * 1024);
        }
        let f = fixture(RuntimeType::Grok, None, false, Some(&soul)).await;
        let sid = "discord:continuity-oversized";
        seed_compacted(&f, sid).await;
        turn(&f, sid, "TURN-NOW").await;
        let seen = f.log.of(RuntimeType::Grok);
        assert_eq!(seen.len(), 1);
        assert!(seen[0].system_prompt.len() > 64 * 1024, "fixture must exceed the old cut");
        let payload = seen[0].payload.as_ref().unwrap();
        assert!(payload.len() <= crate::runtime::prompt_budget::SINGLE_ARGUMENT_PAYLOAD_BUDGET, "{}", payload.len());
        for marker in ["ROLLING-SUMMARY-MARKER", "PINNED-MARKER", "TURN-NOW"] {
            assert!(payload.contains(marker), "{marker} lost from the Grok argument (repeat={repeat})");
        }
        for row in ["OLD-ROW-6", "OLD-ROW-7", "OLD-ROW-8"] {
            assert_eq!(payload.contains(row), rows_kept, "{row} (repeat={repeat})");
        }
    }
}

/// (1)/(2) for a Claude employee, at the last seam before the Claude CLI
/// branch (the CLI then folds the history into its prompt; the dispatch
/// `ClaudeRuntime` path is pinned by `runtime/claude.rs`).
#[tokio::test(flavor = "current_thread")]
async fn claude_employee_turn_carries_history_summary_and_pinned() {
    let f = fixture(RuntimeType::Claude, None, false, None).await;
    let sid = "discord:continuity-claude";
    seed_compacted(&f, sid).await;
    let captured = super::compression_after_cancel::CAPTURE_PROMPT.scope((), Box::pin(crate::channel_reply::build_reply_with_session_inner(
        "TURN-NOW", &f.ctx, Some("worker"), sid, "continuity-user", None,
    ))).await;
    let captured: serde_json::Value = serde_json::from_str(&captured)
        .unwrap_or_else(|error| panic!("prompt boundary was not reached: {error}; reply={captured}"));
    let prompt = captured["prompt"].as_str().unwrap();
    assert!(prompt.contains("ROLLING-SUMMARY-MARKER") && prompt.contains("PINNED-MARKER"), "{prompt}");
    let history = captured["history"].to_string();
    for k in 6..=8 { assert!(history.contains(&format!("OLD-ROW-{k}")), "{history}"); }
}
