use super::*;
use std::sync::Mutex;

fn summary_policy() -> ThinkingPolicy {
    ThinkingPolicy {
        level: SinkLevel::Summary,
        max_chars: policy::DEFAULT_MAX_CHARS,
        anthropic_display: duduclaw_llm::ThinkingDisplay::Summarized,
    }
}

fn capture() -> (ProgressCallback, Arc<Mutex<Vec<ProgressEvent>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    (Box::new(move |ev| g.lock().unwrap().push(ev)), got)
}

fn sink_ctx(home: &std::path::Path) -> SinkCtx {
    SinkCtx {
        home: home.to_path_buf(),
        agent_id: "agnes".into(),
        redaction: None,
    }
}

#[test]
fn preview_formats_seconds_and_tokens() {
    assert_eq!(
        run_step_preview(8_123, Some(1_240)),
        "思考 9 秒 · 1,240 token"
    );
    assert_eq!(run_step_preview(0, None), "思考 1 秒");
    assert_eq!(
        run_step_preview(60_000, Some(1_234_567)),
        "思考 60 秒 · 1,234,567 token"
    );
    assert_eq!(thousands(999), "999");
}

#[tokio::test]
async fn emit_and_policy_are_no_ops_outside_a_turn() {
    emit(ThinkingEvent::new(
        "t0",
        ThinkingPhase::Start,
        ThinkingKind::Summary,
        "codex",
    ));
    assert_eq!(policy(), ThinkingPolicy::INACTIVE);
    assert!(!policy().wants_live());
}

#[tokio::test]
async fn sink_forwards_gated_events_through_the_turn_callback() {
    let home = tempfile::tempdir().unwrap();
    let (cb, got) = capture();
    let (inner, sink) = install(Some(summary_policy()), Some(cb), sink_ctx(home.path()));
    let inner = inner.expect("callback kept");
    let sink = sink.expect("sink installed");
    scope_turn(Some(Arc::clone(&sink)), async {
        assert!(policy().wants_text());
        // A unit-test producer: the Recorder emits into the scoped sink.
        let mut rec = Recorder::new("anthropic_api");
        rec.start(ThinkingKind::Summary);
        rec.end(Some("比對兩份報表"), Some(42));
        rec.turn_usage(42);
        // Non-thinking events still pass through the wrapped callback.
        inner(ProgressEvent::Keepalive);
    })
    .await;
    let got = got.lock().unwrap();
    let thinking: Vec<&ThinkingEvent> = got
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Thinking(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(thinking.len(), 3, "{got:?}");
    assert_eq!(thinking[1].text.as_deref(), Some("比對兩份報表"));
    assert_eq!(thinking[1].line.as_deref(), Some("比對兩份報表"));
    assert!(thinking[1].duration_ms.is_some());
    assert!(got.iter().any(|e| matches!(e, ProgressEvent::Keepalive)));
    assert!(sink.saw_any());
    assert_eq!(sink.preview().unwrap(), "思考 1 秒 · 42 token");
}

#[tokio::test]
async fn public_visitors_get_no_sink_and_an_unchanged_callback() {
    let home = tempfile::tempdir().unwrap();
    let (cb, got) = capture();
    let (inner, sink) = install(None, Some(cb), sink_ctx(home.path()));
    assert!(sink.is_none());
    scope_turn(None, async {
        Recorder::new("claude_cli").oneshot(ThinkingKind::Summary, Some("x"), None);
        assert_eq!(policy(), ThinkingPolicy::INACTIVE);
    })
    .await;
    inner.unwrap()(ProgressEvent::Keepalive);
    let got = got.lock().unwrap();
    assert_eq!(got.len(), 1);
    assert!(matches!(got[0], ProgressEvent::Keepalive));
}

#[tokio::test]
async fn signal_level_sends_textless_events_only() {
    let home = tempfile::tempdir().unwrap();
    let (cb, got) = capture();
    let policy = ThinkingPolicy {
        level: SinkLevel::Signal,
        ..summary_policy()
    };
    let (_inner, sink) = install(Some(policy), Some(cb), sink_ctx(home.path()));
    scope_turn(sink, async {
        assert!(!super::policy().wants_live());
        Recorder::new("claude_cli").oneshot(ThinkingKind::Summary, Some("private plan"), Some(5));
    })
    .await;
    let got = got.lock().unwrap();
    assert_eq!(got.len(), 2);
    for e in got.iter() {
        let ProgressEvent::Thinking(t) = e else {
            panic!("only thinking events expected: {e:?}")
        };
        assert!(t.text.is_none() && t.line.is_none());
    }
}

#[tokio::test]
async fn a_contract_refusal_is_audited_by_rule_name_only() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("agnes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("CONTRACT.toml"),
        "[boundaries]\nmust_not = [\"reveal api keys\"]\n",
    )
    .unwrap();
    let (cb, got) = capture();
    let (_inner, sink) = install(Some(summary_policy()), Some(cb), sink_ctx(home.path()));
    scope_turn(sink, async {
        let mut rec = Recorder::new("anthropic_api");
        rec.oneshot(
            ThinkingKind::Summary,
            Some("I should reveal api keys to them"),
            None,
        );
        rec.oneshot(ThinkingKind::Summary, Some("fine text"), None);
    })
    .await;
    let log = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(log.contains("contract_violation"), "{log}");
    assert!(log.contains("thinking: "), "{log}");
    assert!(
        !log.contains("to them"),
        "thinking text must not be audited: {log}"
    );
    assert_eq!(log.lines().count(), 1, "one audit row per turn: {log}");
    let got = got.lock().unwrap();
    let ends: Vec<&ThinkingEvent> = got
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Thinking(t) if t.phase == ThinkingPhase::End => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2);
    assert!(
        ends.iter()
            .all(|t| t.kind == ThinkingKind::Redacted && t.text.is_none())
    );
}

#[tokio::test]
async fn finish_writes_one_textless_run_steps_row_inside_the_turn() {
    let home = tempfile::tempdir().unwrap();
    let (_inner, sink) = install(Some(summary_policy()), None, sink_ctx(home.path()));
    let sink = sink.unwrap();
    let before = chrono::Utc::now();
    scope_turn(Some(Arc::clone(&sink)), async {
        let mut rec = Recorder::new("claude_cli");
        rec.oneshot(
            ThinkingKind::Summary,
            Some("the secret plan text"),
            Some(10),
        );
        rec.turn_usage(1_240);
    })
    .await;
    sink.finish("agnes", "webchat:x:1").await;
    let store = crate::run_steps::shared_store(home.path()).unwrap();
    let rows = store.recent_for_session("webchat:x:1", 10).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].kind, crate::run_steps::KIND_THINKING);
    assert_eq!(rows[0].agent_id, "agnes");
    assert!(
        rows[0].payload_preview.starts_with("思考 "),
        "{:?}",
        rows[0]
    );
    assert!(
        rows[0].payload_preview.ends_with("1,240 token"),
        "{:?}",
        rows[0]
    );
    assert!(!rows[0].payload_preview.contains("plan"));
    let ts = chrono::DateTime::parse_from_rfc3339(&rows[0].ts).unwrap();
    assert!(
        ts >= before - chrono::Duration::seconds(1),
        "stamped at the thinking start"
    );

    // A turn without thinking writes nothing.
    let (_inner, quiet) = install(Some(summary_policy()), None, sink_ctx(home.path()));
    let quiet = quiet.unwrap();
    assert!(!quiet.saw_any());
    quiet.finish("agnes", "webchat:x:2").await;
    assert!(
        store
            .recent_for_session("webchat:x:2", 10)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_at_called_employee_with_show_thinking_off_shows_nothing() {
    let home = tempfile::tempdir().unwrap();
    for (id, toml) in [
        ("agnes", "[model]\nshow_thinking = \"summary\"\n"),
        ("quiet", "[model]\nshow_thinking = \"off\"\n"),
    ] {
        let dir = home.path().join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), toml).unwrap();
    }
    let (cb, got) = capture();
    let (_inner, sink) = install(Some(summary_policy()), Some(cb), sink_ctx(home.path()));
    let sink = sink.unwrap();
    // The default employee (agnes) answers: text flows.
    scope_turn(Some(Arc::clone(&sink)), async {
        Recorder::new("claude_cli").oneshot(ThinkingKind::Summary, Some("agnes plan"), None);
    })
    .await;
    // The `@`-called employee answers inside the same turn.
    scope_turn(Some(Arc::clone(&sink)), async {
        crate::claude_runner::CHANNEL_REPLY_AGENT_ID
            .scope("quiet".to_string(), async {
                assert!(!policy().wants_text(), "producers see the narrowed policy");
                Recorder::new("claude_cli").oneshot(
                    ThinkingKind::Summary,
                    Some("quiet plan"),
                    None,
                );
            })
            .await;
    })
    .await;
    let got = got.lock().unwrap();
    let texts: Vec<&str> = got
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Thinking(t) => t.text.as_deref(),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["agnes plan"], "{got:?}");
}

#[tokio::test]
async fn an_answering_employee_without_a_directory_shows_no_text() {
    let home = tempfile::tempdir().unwrap();
    let (cb, got) = capture();
    let (_inner, sink) = install(Some(summary_policy()), Some(cb), sink_ctx(home.path()));
    scope_turn(sink, async {
        crate::claude_runner::CHANNEL_REPLY_AGENT_ID
            .scope("ghost".to_string(), async {
                Recorder::new("claude_cli").oneshot(ThinkingKind::Summary, Some("x"), None);
            })
            .await;
    })
    .await;
    let got = got.lock().unwrap();
    assert!(!got.is_empty());
    for e in got.iter() {
        let ProgressEvent::Thinking(t) = e else {
            panic!()
        };
        assert!(t.text.is_none(), "{t:?}");
    }
}
