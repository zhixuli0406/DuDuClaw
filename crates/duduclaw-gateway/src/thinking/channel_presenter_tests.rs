use super::*;
use crate::thinking::ThinkingPhase;

#[derive(Default)]
struct FakeSurface {
    calls: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl StatusSurface for FakeSurface {
    async fn post(&self, view: &StatusView) -> Option<String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("post:{}", super::super::channel_format::plain(view, 300)));
        Some("m1".into())
    }
    async fn edit(&self, id: &str, view: &StatusView) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("edit:{id}:{}", super::super::channel_format::plain(view, 300)));
    }
    async fn delete(&self, id: &str) {
        self.calls.lock().unwrap().push(format!("delete:{id}"));
    }
}

fn ev(block: &str, phase: ThinkingPhase, text: Option<&str>) -> ThinkingEvent {
    ThinkingEvent {
        text: text.map(str::to_string),
        line: text.map(|t| t.lines().last().unwrap_or("").to_string()),
        ..ThinkingEvent::new(block, phase, ThinkingKind::Summary, "claude_cli")
    }
}

fn collect() -> (ProgressCallback, Arc<Mutex<Vec<ProgressEvent>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    (Box::new(move |e| g.lock().unwrap().push(e)), got)
}

#[tokio::test]
async fn editable_status_is_posted_once_throttled_and_edited_into_the_summary() {
    let surface = Arc::new(FakeSurface::default());
    let p = ChannelThinking::new("telegram", false, Lang::ZhTw, Some(surface.clone()));
    p.set_show(ShowThinking::Summary);
    let (cb, got) = collect();
    let cb = p.wrap(cb);
    cb(ProgressEvent::Thinking(ev("t0", ThinkingPhase::Start, None)));
    tokio::task::yield_now().await;
    cb(ProgressEvent::Thinking(ev("t0", ThinkingPhase::Update, Some("比對報表"))));
    // Many updates within one interval ⇒ at most one edit per interval.
    for _ in 0..5 {
        cb(ProgressEvent::Thinking(ev("t0", ThinkingPhase::Update, Some("比對報表\n寄出前確認"))));
    }
    tokio::time::sleep(EDIT_INTERVAL + Duration::from_millis(10)).await;
    // The interim line is held back while the status shows.
    cb(ProgressEvent::Interim {
        text: "💭 已收到".into(),
    });
    cb(ProgressEvent::Thinking(ev("t0", ThinkingPhase::End, Some("比對報表\n寄出前確認"))));
    assert!(p.finish().await.is_none(), "editable ⇒ no footer");
    let calls = surface.calls.lock().unwrap().clone();
    assert_eq!(calls.iter().filter(|c| c.starts_with("post:")).count(), 1, "{calls:?}");
    assert!(calls[0].starts_with("post:💭 思考中…"), "{calls:?}");
    let last = calls.last().unwrap();
    assert!(last.starts_with("edit:m1:💭 思考了"), "{last}");
    assert!(last.ends_with("比對報表\n寄出前確認"), "{last}");
    assert!(calls.len() <= 3, "throttled: {calls:?}");
    // Thinking events still reach the channel callback (which drops them);
    // the interim line did not.
    let got = got.lock().unwrap();
    assert!(got.iter().all(|e| !matches!(e, ProgressEvent::Interim { .. })));
}

#[tokio::test]
async fn status_audience_never_sees_text() {
    let surface = Arc::new(FakeSurface::default());
    let p = ChannelThinking::new("telegram", false, Lang::ZhTw, Some(surface.clone()));
    p.set_show(ShowThinking::Status);
    p.observe(&ev("t0", ThinkingPhase::End, Some("secret plan")));
    p.finish().await;
    let calls = surface.calls.lock().unwrap().clone();
    assert!(calls.iter().all(|c| !c.contains("secret")), "{calls:?}");
    assert!(calls.last().unwrap().contains("思考了"), "{calls:?}");
}

#[tokio::test]
async fn off_or_no_thinking_shows_nothing() {
    for show in [ShowThinking::Off, ShowThinking::Summary] {
        let surface = Arc::new(FakeSurface::default());
        let p = ChannelThinking::new("telegram", false, Lang::ZhTw, Some(surface.clone()));
        p.set_show(show);
        if show == ShowThinking::Off {
            p.observe(&ev("t0", ThinkingPhase::End, Some("x")));
        }
        assert!(p.finish().await.is_none());
        assert!(surface.calls.lock().unwrap().is_empty(), "{show:?}");
    }
    // Without a presenter installed by the channel nothing is in scope.
    assert!(current().is_none());
}

#[tokio::test]
async fn non_editable_platforms_get_a_footer() {
    let p = ChannelThinking::new("whatsapp", false, Lang::ZhTw, None);
    p.set_show(ShowThinking::Summary);
    let (cb, got) = collect();
    let cb = p.wrap(cb);
    cb(ProgressEvent::Thinking(ev("t0", ThinkingPhase::End, Some("先想一下"))));
    // The interim line stays on non-editable platforms.
    cb(ProgressEvent::Interim { text: "⏳".into() });
    assert!(got.lock().unwrap().iter().any(|e| matches!(e, ProgressEvent::Interim { .. })));
    let footer = p.finish().await.expect("footer");
    assert_eq!(footer.text(), "💭 思考了 1 秒\n先想一下");
    let reply = with_footer(
        format!("答案{}", crate::channel_reply::product_footer_suffix()),
        Some(&footer),
    );
    assert_eq!(
        reply,
        format!(
            "答案\n\n💭 思考了 1 秒\n先想一下{}",
            crate::channel_reply::product_footer_suffix()
        )
    );
    assert_eq!(with_footer("答案".into(), Some(&footer)), "答案\n\n💭 思考了 1 秒\n先想一下");
    assert_eq!(with_footer(String::new(), Some(&footer)), "");
    assert_eq!(with_footer("答案".into(), None), "答案");
}

#[tokio::test]
async fn redacted_blocks_and_turn_usage_add_no_text() {
    let p = ChannelThinking::new("line", false, Lang::ZhTw, None);
    p.set_show(ShowThinking::Summary);
    // A block shown live, then withheld by the outbound checks: its text
    // leaves the final summary too.
    p.observe(&ev("t0", ThinkingPhase::Update, Some("shown before the refusal")));
    let mut redacted = ev("t0", ThinkingPhase::End, None);
    redacted.kind = ThinkingKind::Redacted;
    p.observe(&redacted);
    let mut usage = ev("turn", ThinkingPhase::End, Some("never"));
    usage.tokens = Some(10);
    p.observe(&usage);
    let footer = p.finish().await.unwrap();
    assert_eq!(footer.text(), "💭 思考了 1 秒");
}

#[tokio::test]
async fn abandon_deletes_the_status_message() {
    let surface = Arc::new(FakeSurface::default());
    let p = ChannelThinking::new("discord", false, Lang::En, Some(surface.clone()));
    p.set_show(ShowThinking::Status);
    p.observe(&ev("t0", ThinkingPhase::Start, None));
    // Let the status task post.
    for _ in 0..20 {
        tokio::task::yield_now().await;
        if !surface.calls.lock().unwrap().is_empty() {
            break;
        }
    }
    p.abandon().await;
    let calls = surface.calls.lock().unwrap().clone();
    assert_eq!(calls.first().map(String::as_str), Some("post:💭 Thinking… 1s"));
    assert_eq!(calls.last().map(String::as_str), Some("delete:m1"));
}

#[tokio::test]
async fn line_footer_message_goes_first_and_never_exceeds_five() {
    let p = ChannelThinking::new("line", false, Lang::ZhTw, None);
    p.set_show(ShowThinking::Status);
    p.observe(&ev("t0", ThinkingPhase::End, Some("hidden")));
    let footer = p.finish().await.unwrap();
    let extra = line_footer_message(&footer);
    assert_eq!(extra["text"], "💭 思考了 1 秒", "timer only for this audience");
    let answer = |n: usize| -> Vec<serde_json::Value> {
        (0..n).map(|i| serde_json::json!({"type": "text", "text": format!("a{i}")})).collect()
    };
    let out = with_reply_extra(answer(1), Some(extra.clone()));
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], extra);
    assert_eq!(out[1]["text"], "a0", "the answer stays last (quick replies attach there)");
    for n in 1..=6 {
        let out = with_reply_extra(answer(n), Some(extra.clone()));
        if n >= LINE_MAX_MESSAGES {
            assert_eq!(out.len(), n, "a full reply gets no footer");
        } else {
            assert_eq!(out.len(), n + 1);
        }
    }
    assert_eq!(with_reply_extra(answer(1), None).len(), 1);
    assert!(with_reply_extra(vec![], Some(extra)).is_empty());
}
