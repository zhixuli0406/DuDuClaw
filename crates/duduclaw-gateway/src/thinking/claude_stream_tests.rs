use super::*;
use crate::thinking::policy::SinkLevel;
use crate::thinking::{ThinkingEvent, ThinkingPhase};
use std::sync::{Arc, Mutex};

fn rig() -> (ClaudeStreamThinking, Arc<Mutex<Vec<ThinkingEvent>>>) {
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    let rec = Recorder::with_emitter("claude_cli", Arc::new(move |ev| g.lock().unwrap().push(ev)));
    (ClaudeStreamThinking::with_recorder(rec), got)
}

fn feed(p: &mut ClaudeStreamThinking, lines: &[&str]) -> Vec<Flow> {
    lines
        .iter()
        .map(|l| p.observe(&serde_json::from_str(l).unwrap()))
        .collect()
}

fn shape(got: &[ThinkingEvent]) -> Vec<(String, &'static str, &'static str)> {
    got.iter()
        .map(|e| (e.block_id.clone(), e.phase.as_str(), e.kind.as_str()))
        .collect()
}

/// The partial-message fixture: a streamed thinking block (with a fake key
/// and a redaction token in the text), its signature, the answer text as
/// `text_delta`s, the complete assistant message, and the result.
pub(crate) const PARTIAL_FIXTURE: &[&str] = &[
    r#"{"type":"system","subtype":"init","model":"claude-opus-5"}"#,
    r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}"#,
    r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":12,"estimated_tokens_delta":12}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"先比對兩份報表，金鑰 sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789 "}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"寄給 <REDACT:EMAIL:0123456789abcdef0123456789abcdef> 之前再確認。\n"}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"SIG-NEVER-SHOWN"}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_stop","index":0}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"兩份報表"}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"一致。"}}}"#,
    r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
    r#"{"type":"assistant","message":{"model":"claude-opus-5","content":[{"type":"thinking","thinking":"先比對兩份報表","signature":"SIG-NEVER-SHOWN"},{"type":"text","text":"兩份報表一致。"}]}}"#,
    r#"{"type":"result","subtype":"success","is_error":false,"result":"兩份報表一致。","usage":{"input_tokens":10,"output_tokens":40,"output_tokens_details":{"thinking_tokens":31}}}"#,
];

#[test]
fn partial_stream_reports_one_block_live_and_skips_its_repeat() {
    let (mut p, got) = rig();
    let flows = feed(&mut p, PARTIAL_FIXTURE);
    p.finish();
    // Every stream_event line is consumed; nothing else is.
    for (line, flow) in PARTIAL_FIXTURE.iter().zip(&flows) {
        let want = if line.contains(r#""type":"stream_event""#) {
            Flow::Consumed
        } else {
            Flow::Continue
        };
        assert_eq!(*flow, want, "{line}");
    }
    let got = got.lock().unwrap();
    assert_eq!(
        shape(&got),
        [
            ("t0".into(), "start", "summary"),
            ("t0".into(), "update", "summary"), // thinking_tokens estimate
            ("t0".into(), "update", "summary"),
            ("t0".into(), "update", "summary"),
            ("t0".into(), "end", "summary"),
            ("turn".into(), "end", "omitted"),
        ]
    );
    assert_eq!(got[1].tokens, Some(12));
    assert_eq!(got[5].tokens, Some(31));
    let all_text: String = got.iter().filter_map(|e| e.text.clone()).collect();
    assert!(!all_text.contains("SIG-NEVER-SHOWN"));
    assert!(!all_text.contains("兩份報表一致"), "text_delta never becomes thinking");
}

#[test]
fn without_partial_messages_assistant_blocks_are_oneshots() {
    let (mut p, got) = rig();
    feed(
        &mut p,
        &[
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"","signature":"S"},{"type":"redacted_thinking","data":"CIPHERTEXT"},{"type":"thinking","thinking":"規劃步驟"},{"type":"text","text":"好"}]}}"#,
            r#"{"type":"result","usage":{"output_tokens":5,"output_tokens_details":{"thinking_tokens":0}}}"#,
        ],
    );
    p.finish();
    let got = got.lock().unwrap();
    assert_eq!(
        shape(&got),
        [
            ("t0".into(), "start", "omitted"),
            ("t0".into(), "end", "omitted"),
            ("t1".into(), "start", "redacted"),
            ("t1".into(), "end", "redacted"),
            ("t2".into(), "start", "summary"),
            ("t2".into(), "end", "summary"),
        ],
        "zero thinking_tokens emits no turn usage"
    );
    assert_eq!(got[5].text.as_deref(), Some("規劃步驟"));
    assert!(got.iter().all(|e| e.text.as_deref() != Some("CIPHERTEXT")));
}

#[test]
fn a_token_estimate_opens_the_block_the_assistant_message_then_closes() {
    let (mut p, got) = rig();
    feed(
        &mut p,
        &[
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":100}"#,
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":250}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":""},{"type":"text","text":"x"}]}}"#,
            r#"{"type":"system","subtype":"init"}"#,
        ],
    );
    p.finish();
    let got = got.lock().unwrap();
    assert_eq!(
        shape(&got),
        [
            ("t0".into(), "start", "omitted"),
            ("t0".into(), "update", "omitted"),
            ("t0".into(), "update", "omitted"),
            ("t0".into(), "end", "omitted"),
        ]
    );
    assert_eq!(got[2].tokens, Some(250));
}

#[test]
fn an_unterminated_streamed_block_is_closed_at_the_end() {
    let (mut p, got) = rig();
    feed(
        &mut p,
        &[
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":3,"delta":{"type":"thinking_delta","thinking":"other index"}}}"#,
        ],
    );
    p.finish();
    let got = got.lock().unwrap();
    assert_eq!(
        shape(&got),
        [("t0".into(), "start", "summary"), ("t0".into(), "end", "omitted")]
    );
    assert!(got.iter().all(|e| e.phase != ThinkingPhase::Update));
}

#[test]
fn extra_args_only_when_text_is_read_live() {
    let at = |level| ThinkingPolicy {
        level,
        ..ThinkingPolicy::INACTIVE
    };
    assert_eq!(extra_args(&at(SinkLevel::Summary)), [PARTIAL_MESSAGES_FLAG]);
    for level in [SinkLevel::Off, SinkLevel::Signal, SinkLevel::Status] {
        assert!(extra_args(&at(level)).is_empty(), "{level:?}");
    }
    assert!(extra_args(&ThinkingPolicy::INACTIVE).is_empty());
}

#[test]
fn debug_log_elides_partial_messages_and_thinking() {
    for line in PARTIAL_FIXTURE {
        let parsed: Value = serde_json::from_str(line).unwrap();
        let out = debug_log_line(line, Some(&parsed));
        if line.contains("thinking_delta") {
            assert_eq!(out, r#"{"type":"stream_event","elided":"thinking_delta"}"#);
        } else if is_stream_event(&parsed) {
            assert_eq!(out, r#"{"type":"stream_event","elided":"partial_message"}"#);
        } else {
            assert_eq!(out, diagnostic_line(line, Some(&parsed)).unwrap());
        }
    }
    assert_eq!(debug_log_line("not json", None), "not json");
}

fn ev_line(v: serde_json::Value) -> Value {
    v
}

#[tokio::test]
async fn a_retry_after_an_aborted_attempt_gets_new_ids_and_both_blocks_close() {
    use crate::channel_reply::{ProgressCallback, ProgressEvent};
    let home = tempfile::tempdir().unwrap();
    let got = Arc::new(Mutex::new(Vec::new()));
    let g = Arc::clone(&got);
    let cb: ProgressCallback = Box::new(move |e| {
        if let ProgressEvent::Thinking(t) = e {
            g.lock().unwrap().push(t);
        }
    });
    let policy = ThinkingPolicy {
        level: SinkLevel::Summary,
        max_chars: crate::thinking::policy::DEFAULT_MAX_CHARS,
        ..ThinkingPolicy::INACTIVE
    };
    let ctx = crate::thinking::SinkCtx {
        home: home.path().to_path_buf(),
        agent_id: "agnes".into(),
        redaction: None,
    };
    let (_cb, sink) = crate::thinking::install(Some(policy), Some(cb), ctx);
    crate::thinking::scope_turn(sink, async {
        let start = ev_line(serde_json::json!({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}));
        let delta = |t: &str| ev_line(serde_json::json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":t}}}));
        // Attempt 1 dies mid-thinking (error return ⇒ the parser is dropped).
        {
            let mut a1 = ClaudeStreamThinking::new();
            a1.observe(&start);
            a1.observe(&delta("第一次嘗試。\n"));
        }
        // Attempt 2 (retry / failover) thinks again from index 0.
        let mut a2 = ClaudeStreamThinking::new();
        a2.observe(&start);
        a2.observe(&delta("第二次嘗試。\n"));
        a2.observe(&ev_line(serde_json::json!({"type":"stream_event","event":{"type":"content_block_stop","index":0}})));
    })
    .await;
    let got = got.lock().unwrap();
    let ids: Vec<&str> = got.iter().map(|e| e.block_id.as_str()).collect();
    let starts: Vec<&str> = got.iter().filter(|e| e.phase == ThinkingPhase::Start).map(|e| e.block_id.as_str()).collect();
    let ends: Vec<&str> = got.iter().filter(|e| e.phase == ThinkingPhase::End).map(|e| e.block_id.as_str()).collect();
    assert_eq!(starts, ["t0", "t1"], "{ids:?}");
    assert_eq!(ends, ["t0", "t1"], "both blocks closed: {ids:?}");
    let last_t1 = got.iter().rev().find(|e| e.block_id == "t1" && e.text.is_some()).unwrap();
    assert!(!last_t1.text.as_deref().unwrap().contains("第一次"), "attempts never merge");
}

#[test]
fn subagent_lines_never_touch_the_main_blocks() {
    let (mut p, got) = rig();
    let flows = feed(
        &mut p,
        &[
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_1","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}"#,
            r#"{"type":"stream_event","parent_tool_use_id":"toolu_1","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"subagent plan"}}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"toolu_1","message":{"content":[{"type":"thinking","thinking":"subagent plan"}]}}"#,
            r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}}"#,
            r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_stop","index":0}}"#,
        ],
    );
    assert_eq!(flows[0], Flow::Consumed, "still kept away from the other collectors");
    p.finish();
    let got = got.lock().unwrap();
    assert_eq!(shape(&got), [("t0".into(), "start", "summary"), ("t0".into(), "end", "omitted")]);
    assert!(got.iter().all(|e| e.text.is_none()));
}

#[test]
fn an_aborted_streamed_message_never_suppresses_a_later_block() {
    let (mut p, got) = rig();
    feed(
        &mut p,
        &[
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"X"}}}"#,
            // m1's assistant repeat never arrives (aborted); m2 is not streamed.
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}}}"#,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"thinking","thinking":"第二則訊息的思考"}]}}"#,
            // A late repeat of m1 is still skipped.
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"redacted_thinking","data":"X"}]}}"#,
        ],
    );
    p.finish();
    let got = got.lock().unwrap();
    assert_eq!(
        shape(&got),
        [
            ("t0".into(), "start", "redacted"),
            ("t0".into(), "end", "redacted"),
            ("t1".into(), "start", "summary"),
            ("t1".into(), "end", "summary"),
        ]
    );
    assert_eq!(got[3].text.as_deref(), Some("第二則訊息的思考"));
}

#[test]
fn diagnostics_never_keep_thinking_text() {
    let assistant = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"SECRET PLAN","signature":"S"},{"type":"redacted_thinking","data":"CIPHER"},{"type":"text","text":"答"}]}}"#;
    let parsed: Value = serde_json::from_str(assistant).unwrap();
    let kept = diagnostic_line(assistant, Some(&parsed)).unwrap();
    assert!(!kept.contains("SECRET PLAN") && !kept.contains("CIPHER") && !kept.contains("\"S\""), "{kept}");
    assert!(kept.contains("\"elided\":true") && kept.contains("答"), "{kept}");
    let partial = PARTIAL_FIXTURE[4];
    let parsed: Value = serde_json::from_str(partial).unwrap();
    assert_eq!(diagnostic_line(partial, Some(&parsed)), None);
    let result = PARTIAL_FIXTURE.last().unwrap();
    let parsed: Value = serde_json::from_str(result).unwrap();
    assert_eq!(diagnostic_line(result, Some(&parsed)).as_deref(), Some(*result));
    assert_eq!(diagnostic_line("not json", None).as_deref(), Some("not json"));
}

// Written for F1; not run (the review task prohibits builds/tests).
#[test]
fn debug_and_diagnostic_sinks_mask_credentials_and_omit_fragments() {
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"password: fixture-reply"},{"type":"tool_use","input":{"password":"fixture-argument"}}]}}"#;
    let value: Value = serde_json::from_str(line).unwrap();
    for stored in [debug_log_line(line, Some(&value)).into_owned(), diagnostic_line(line, Some(&value)).unwrap()] {
        assert!(!stored.contains("fixture-"));
        assert!(serde_json::from_str::<Value>(&stored).is_ok());
    }
    let delta = r#"{"type":"stream_event","event":{"delta":{"type":"text_delta","text":"fixture-fragment"}}}"#;
    let value: Value = serde_json::from_str(delta).unwrap();
    assert!(!debug_log_line(delta, Some(&value)).contains("fixture-fragment"));
    assert_eq!(diagnostic_line(delta, Some(&value)), None);
}
