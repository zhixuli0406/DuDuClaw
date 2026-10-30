use super::*;
use serde_json::json;
fn frame(value: serde_json::Value) -> Frame {
    Frame::parse(&value.to_string()).unwrap()
}
fn connected() -> ChatState {
    let mut state = ChatState::default();
    state.apply(frame(
        json!({"type":"session_info","session_id":"webchat:webchat:owner:connection",
        "agent_name":"DuDu","agent_icon":"","supports_vision":false,"model":"test"}),
    ));
    state
}
#[test]
fn real_frames_and_additive_cancel() {
    let mut s = connected();
    let send = s.prepare(submission("你好", Some("adu")).unwrap()).unwrap();
    let sent = serde_json::to_value(&send).unwrap();
    assert_eq!(sent["type"], "user_message");
    assert_eq!(sent["agent"], "adu");
    assert!(sent.get("session_id").is_none());
    // Shapes copied from webchat.rs ChatMessage and its documented examples.
    for value in [
        json!({"type":"progress","content":"working","kind":"tool","tool":"Read","detail":"file","conv":s.conv}),
        json!({"type":"step","phase":"start","tool":"Bash","summary":"cargo test","depth":1,"ts":1720598400456u64}),
        json!({"type":"thinking","phase":"update","block":"t0","kind":"summary","text":"secret","line":"comparing","started_at":1729500000123u64,"tokens":1240,"truncated":false}),
    ] {
        s.apply(frame(value));
        assert!(s.running);
    }
    assert_eq!(s.lines.len(), 1);
    assert!(!s.can_cancel());
    s.apply(frame(json!({"type":"progress","content":"new gateway","session_id":"s1","conv":s.conv,"msg_id":s.msg_id})));
    assert!(s.can_cancel());
    assert_eq!(s.cancel_frame().unwrap()["msg_id"], sent["msg_id"]);
    s.apply(frame(json!({"type":"cancel_accepted","msg_id":s.msg_id})));
    assert!(s.running);
    s.apply(frame(json!({"type":"cancelled","msg_id":s.msg_id})));
    assert!(!s.running);
    s.prepare(submission("/status", Some("adu")).unwrap())
        .unwrap();
    s.apply(frame(json!({"type":"assistant_done","content":"done","tokens_used":12,"conv":s.conv,"model":"test","agent":"adu"})));
    assert!(!s.running);
    assert_eq!(s.lines.back().unwrap().plain(), "員工: done");
    s.prepare(submission("again", Some("adu")).unwrap())
        .unwrap();
    s.apply(frame(
        json!({"type":"error","message":"too many","code":"turn_limit"}),
    ));
    assert!(!s.running);
    assert!(s.status.is(strings::CHAT_TURN_LIMIT));
}
#[test]
fn old_gateway_resume_and_attribution() {
    let mut s = connected();
    s.prepare(submission("first", Some("adu")).unwrap())
        .unwrap();
    s.apply(frame(
        json!({"type":"assistant_done","content":"wrong","tokens_used":0,"conv":"other"}),
    ));
    assert!(s.running);
    s.apply(frame(
        json!({"type":"assistant_done","content":"correct","tokens_used":0}),
    ));
    let resumed = s
        .prepare(submission("second", Some("adu")).unwrap())
        .unwrap();
    assert_eq!(
        resumed.session_id,
        Some(format!(
            "webchat:webchat:owner:connection#agent:adu#conv:{}",
            s.conv
        ))
    );
    s.disconnected();
    assert!(!s.running);
    assert!(s.stale);
    assert!(!s.cancel_supported);
}
#[test]
fn transcript_is_bounded_and_cannot_emit_escape_codes() {
    let mut s = connected();
    s.append("assistant", &"界".repeat(MAX_LINES * MAX_LINE_CHARS + 1));
    assert_eq!(s.lines.len(), MAX_LINES);
    assert!(
        s.lines
            .iter()
            .all(|l| l.plain().chars().count() <= MAX_LINE_CHARS)
    );
    s.append(
        "assistant",
        "\x1b]8;;https://evil\x07click\x1b]8;;\x07\u{202e}\u{200b}\x1b]52;c;bad\x07",
    );
    assert!(
        s.lines
            .iter()
            .all(|l| !l.plain().chars().any(crate::sanitize::forbidden))
    );
}
#[test]
fn mention_and_computer_commands() {
    assert_eq!(
        submission("@小藍 你好世界", Some("adu")).unwrap(),
        Submission {
            agent: "小藍".into(),
            content: "你好世界".into()
        }
    );
    assert!(submission("@adu", Some("other")).is_none());
    for command in ["/pause", "/stop now", "/resume", "@adu /pause"] {
        assert_eq!(command_note(command), Some(strings::COMPUTER_COMMAND_NOTE));
    }
    for command in ["/goal 測試", "/status", "/cost", "/stopper"] {
        assert!(command_note(command).is_none());
    }
}
