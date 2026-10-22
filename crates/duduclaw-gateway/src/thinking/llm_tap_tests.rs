use super::*;
use crate::channel_reply::{ProgressCallback, ProgressEvent};
use crate::thinking::policy::{SinkLevel, ThinkingPolicy};
use crate::thinking::{SinkCtx, ThinkingEvent, install, scope_turn};
use duduclaw_llm::{NormalizedUsage, StopReason};
use std::sync::{Arc, Mutex};

fn resp(parts: Vec<ContentPart>) -> ChatResponse {
    ChatResponse {
        parts,
        stop: StopReason::EndTurn,
        usage: NormalizedUsage::default(),
        model_used: "m".into(),
        provider: "p".into(),
    }
}

/// Answers `complete()` with `parts` and `stream()` with `events`.
struct Scripted {
    parts: Vec<ContentPart>,
    events: Vec<StreamEvent>,
    calls: Mutex<Vec<&'static str>>,
}

#[async_trait]
impl ChatProvider for Scripted {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn complete(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.calls.lock().unwrap().push("complete");
        Ok(resp(self.parts.clone()))
    }
    async fn stream(
        &self,
        _req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        self.calls.lock().unwrap().push("stream");
        let evs: Vec<Result<StreamEvent, LlmError>> =
            self.events.clone().into_iter().map(Ok).collect();
        Ok(Box::pin(futures_util::stream::iter(evs)))
    }
}

fn scripted(parts: Vec<ContentPart>, events: Vec<StreamEvent>) -> Scripted {
    Scripted {
        parts,
        events,
        calls: Mutex::new(Vec::new()),
    }
}

fn collecting() -> (Recorder, Arc<Mutex<Vec<ThinkingEvent>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    (
        Recorder::with_emitter("openai_compat", Arc::new(move |ev| g.lock().unwrap().push(ev))),
        got,
    )
}

fn shape(got: &[ThinkingEvent]) -> Vec<(String, &'static str, &'static str)> {
    got.iter()
        .map(|e| (e.block_id.clone(), e.phase.as_str(), e.kind.as_str()))
        .collect()
}

#[tokio::test]
async fn oneshot_reports_parts_and_returns_them_untouched() {
    let parts = vec![
        ContentPart::Reasoning {
            text: "先算 2+2".into(),
            signature: Some("SIG".into()),
        },
        ContentPart::RedactedReasoning {
            data: "CIPHER".into(),
        },
        ContentPart::Reasoning {
            text: String::new(),
            signature: None,
        },
        ContentPart::Text("4".into()),
    ];
    let (rec, got) = collecting();
    let tap = ReasoningTap::with_recorder(
        scripted(parts.clone(), vec![]),
        rec,
        ThinkingKind::Summary,
        true,
    );
    // No sink in scope ⇒ never the streaming path, even for a live tap.
    let out = tap.complete(&ChatRequest::new("m")).await.unwrap();
    assert_eq!(out.parts, parts, "the loop gets exactly what the provider returned");
    assert_eq!(*tap.inner.calls.lock().unwrap(), ["complete"]);
    let got = got.lock().unwrap();
    assert_eq!(
        shape(&got),
        [
            ("t0".into(), "start", "summary"),
            ("t0".into(), "end", "summary"),
            ("t1".into(), "start", "redacted"),
            ("t1".into(), "end", "redacted"),
            ("t2".into(), "start", "omitted"),
            ("t2".into(), "end", "omitted"),
        ]
    );
    assert_eq!(got[1].text.as_deref(), Some("先算 2+2"));
    assert!(got.iter().all(|e| e.text.as_deref() != Some("SIG")));
}

#[tokio::test]
async fn raw_tap_reports_a_leading_inline_think_block() {
    let (rec, got) = collecting();
    let tap = ReasoningTap::with_recorder(
        scripted(vec![ContentPart::Text("<think>想一下</think>答案".into())], vec![]),
        rec,
        ThinkingKind::Raw,
        false,
    );
    let out = tap.complete(&ChatRequest::new("m")).await.unwrap();
    assert_eq!(out.text(), "<think>想一下</think>答案", "answer text unchanged here");
    let got = got.lock().unwrap();
    assert_eq!(shape(&got), [("t0".into(), "start", "raw"), ("t0".into(), "end", "raw")]);
    assert_eq!(got[1].text.as_deref(), Some("想一下"));
}

fn capture() -> (ProgressCallback, Arc<Mutex<Vec<ProgressEvent>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    (Box::new(move |ev| g.lock().unwrap().push(ev)), got)
}

fn sink_at(
    level: SinkLevel,
    home: &std::path::Path,
) -> (Arc<crate::thinking::ThinkingSink>, Arc<Mutex<Vec<ProgressEvent>>>) {
    let (cb, got) = capture();
    let policy = ThinkingPolicy {
        level,
        max_chars: crate::thinking::policy::DEFAULT_MAX_CHARS,
        ..ThinkingPolicy::INACTIVE
    };
    let ctx = SinkCtx {
        home: home.to_path_buf(),
        agent_id: "agnes".into(),
        redaction: None,
    };
    let (_inner, sink) = install(Some(policy), Some(cb), ctx);
    (sink.unwrap(), got)
}

#[tokio::test]
async fn live_tap_streams_reasoning_and_returns_the_done_response() {
    let done = resp(vec![
        ContentPart::Reasoning {
            text: "第一步第二步".into(),
            signature: None,
        },
        ContentPart::Text("好".into()),
    ]);
    let events = vec![
        StreamEvent::ReasoningDelta("第一步".into()),
        StreamEvent::ReasoningDelta("第二步".into()),
        StreamEvent::TextDelta("好".into()),
        StreamEvent::Done(done.clone()),
    ];
    let home = tempfile::tempdir().unwrap();
    let (sink, got) = sink_at(SinkLevel::Summary, home.path());
    let tap = ReasoningTap::new(scripted(vec![], events), "anthropic_api", ThinkingKind::Summary, true);
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let out = crate::cost_telemetry::USAGE_ESTIMATED
        .scope(
            flag.clone(),
            scope_turn(Some(sink), tap.complete(&ChatRequest::new("m"))),
        )
        .await
        .unwrap();
    assert_eq!(out.parts, done.parts, "the loop gets the Done parts untouched");
    // The scripted stream carried no usage: an estimate, flagged as such.
    assert!(out.usage.output_tokens > 0);
    assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(*tap.inner.calls.lock().unwrap(), ["stream"]);
    let got = got.lock().unwrap();
    let thinking: Vec<&ThinkingEvent> = got
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Thinking(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(thinking.first().map(|e| e.phase.as_str()), Some("start"));
    let last = thinking.last().unwrap();
    assert_eq!(last.phase.as_str(), "end");
    assert_eq!(last.text.as_deref(), Some("第一步第二步"), "reported once, live");
    assert_eq!(
        thinking.iter().filter(|e| e.phase.as_str() == "start").count(),
        1
    );
}

#[tokio::test]
async fn a_signal_sink_never_switches_to_streaming() {
    let home = tempfile::tempdir().unwrap();
    let (sink, _got) = sink_at(SinkLevel::Signal, home.path());
    let tap = ReasoningTap::new(
        scripted(vec![ContentPart::Text("x".into())], vec![]),
        "openai_compat",
        ThinkingKind::Raw,
        true,
    );
    scope_turn(Some(sink), tap.complete(&ChatRequest::new("m")))
        .await
        .unwrap();
    assert_eq!(*tap.inner.calls.lock().unwrap(), ["complete"]);
}

/// Refuses `stream()`; `complete()` works.
struct NoStream(Scripted);

#[async_trait]
impl ChatProvider for NoStream {
    fn id(&self) -> &str {
        "no-stream"
    }
    async fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.0.complete(req).await
    }
    async fn stream(
        &self,
        _req: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, LlmError>>, LlmError> {
        self.0.calls.lock().unwrap().push("stream-refused");
        Err(LlmError::InvalidRequest("stream_options not supported".into()))
    }
}

#[tokio::test]
async fn a_backend_that_refuses_streaming_still_answers() {
    let home = tempfile::tempdir().unwrap();
    let (sink, _got) = sink_at(SinkLevel::Summary, home.path());
    let tap = ReasoningTap::new(
        NoStream(scripted(vec![ContentPart::Text("答".into())], vec![])),
        "openai_compat",
        ThinkingKind::Raw,
        true,
    );
    let out = scope_turn(Some(sink), tap.complete(&ChatRequest::new("m")))
        .await
        .unwrap();
    assert_eq!(out.text(), "答");
    assert_eq!(*tap.inner.0.calls.lock().unwrap(), ["stream-refused", "complete"]);
}

#[tokio::test]
async fn a_stream_without_done_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    let (sink, _got) = sink_at(SinkLevel::Summary, home.path());
    let tap = ReasoningTap::new(
        scripted(vec![], vec![StreamEvent::ReasoningDelta("x".into())]),
        "openai_compat",
        ThinkingKind::Raw,
        true,
    );
    let err = scope_turn(Some(sink), tap.complete(&ChatRequest::new("m")))
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::Parse(_)), "{err:?}");
}

// ── L2: a mock OpenAI-compatible SSE server (127.0.0.1, no real provider) ──

mod l2 {
    use super::*;
    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::response::IntoResponse;

    #[derive(Clone, Default)]
    struct Seen(Arc<Mutex<Vec<serde_json::Value>>>);

    async fn chat(State(seen): State<Seen>, body: Bytes) -> impl IntoResponse {
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let streaming = req.get("stream").and_then(|v| v.as_bool()) == Some(true);
        seen.0.lock().unwrap().push(req);
        if !streaming {
            let json = serde_json::json!({
                "model": "qwen3",
                "choices": [{"message": {"role": "assistant",
                    "content": "<think>內嵌思考</think>答案", "reasoning_content": "先想 sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 再答"},
                    "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 4}
            });
            return ([("content-type", "application/json")], json.to_string()).into_response();
        }
        let chunks = [
            r#"{"model":"qwen3","choices":[{"delta":{"reasoning_content":"先比對，"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning":"金鑰 sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 不能外流。\n"}}]}"#,
            r#"{"choices":[{"delta":{"content":"<think>內嵌"}}]}"#,
            r#"{"choices":[{"delta":{"content":"推理</think>"}}]}"#,
            r#"{"choices":[{"delta":{"content":"兩份一致。"},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":7}}"#,
        ];
        let mut sse = String::new();
        for c in chunks {
            sse.push_str(&format!("data: {c}\n\n"));
        }
        sse.push_str("data: [DONE]\n\n");
        ([("content-type", "text/event-stream")], sse).into_response()
    }

    async fn serve() -> (String, Seen) {
        let seen = Seen::default();
        let router = Router::new()
            .route("/v1/chat/completions", axum::routing::post(chat))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (base, seen)
    }

    fn provider(base: &str) -> duduclaw_llm::providers::OpenAiCompatProvider {
        let auth = duduclaw_llm::ApiAuth::new("test-key".to_string()).with_base_url(base.to_string());
        duduclaw_llm::providers::OpenAiCompatProvider::new("vllm".to_string(), auth, base.to_string())
    }

    #[tokio::test]
    async fn sse_reasoning_reaches_a_summary_viewer_masked_and_the_reply_has_no_think_tags() {
        let (base, seen) = serve().await;
        let home = tempfile::tempdir().unwrap();
        // Redaction must be on for raw reasoning to be shown at all.
        let (cb, got) = capture();
        let policy = ThinkingPolicy {
            level: SinkLevel::Summary,
            max_chars: crate::thinking::policy::DEFAULT_MAX_CHARS,
            ..ThinkingPolicy::INACTIVE
        };
        let cfg: duduclaw_redaction::RedactionConfig =
            toml::from_str("enabled = true\nprofiles = [\"general\"]\n").unwrap();
        let manager = Arc::new(
            duduclaw_redaction::RedactionManager::open(
                cfg,
                duduclaw_redaction::ManagerPaths::under_home(home.path()),
            )
            .unwrap(),
        );
        let ctx = SinkCtx {
            home: home.path().to_path_buf(),
            agent_id: "agnes".into(),
            redaction: Some(manager),
        };
        let (_inner, sink) = install(Some(policy), Some(cb), ctx);
        let tap = ReasoningTap::new(provider(&base), "openai_compat", ThinkingKind::Raw, true);
        let out = scope_turn(sink, tap.complete(&ChatRequest::new("qwen3")))
            .await
            .unwrap();
        let reply = duduclaw_core::think_tags::strip_leading_think_block(&out.text());
        assert_eq!(reply, "兩份一致。");
        assert!(!reply.contains("<think>"));
        assert_eq!(seen.0.lock().unwrap()[0]["stream"], true);
        let got = got.lock().unwrap();
        let texts: Vec<String> = got
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Thinking(t) => t.text.clone(),
                _ => None,
            })
            .collect();
        let last = texts.last().expect("thinking text shown");
        assert!(last.contains("先比對"), "{texts:?}");
        assert!(last.contains("內嵌推理"), "{texts:?}");
        assert!(
            texts.iter().all(|t| !t.contains("sk-ant-api03-abcdef")),
            "secrets masked: {texts:?}"
        );
    }

    #[tokio::test]
    async fn without_a_sink_the_request_is_the_plain_non_streaming_one() {
        let (base, seen) = serve().await;
        let tap = ReasoningTap::new(provider(&base), "openai_compat", ThinkingKind::Raw, true);
        let plain = provider(&base);
        let mut req = ChatRequest::new("qwen3");
        req.messages.push(duduclaw_llm::ChatMessage::user("hi".to_string()));
        let a = tap.complete(&req).await.unwrap();
        let b = plain.complete(&req).await.unwrap();
        assert_eq!(a, b);
        let seen = seen.0.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1], "byte-identical request bodies");
        assert_ne!(seen[0].get("stream"), Some(&serde_json::json!(true)));
    }
}
