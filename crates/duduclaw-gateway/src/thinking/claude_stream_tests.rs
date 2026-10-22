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
fn debug_log_elides_thinking_deltas_only() {
    for line in PARTIAL_FIXTURE {
        let parsed: Value = serde_json::from_str(line).unwrap();
        let out = debug_log_line(line, Some(&parsed));
        if line.contains("thinking_delta") {
            assert_eq!(out, r#"{"type":"stream_event","elided":"thinking_delta"}"#);
        } else {
            assert_eq!(out, *line);
        }
    }
    assert_eq!(debug_log_line("not json", None), "not json");
}
