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
    assert!(s.lines.len() <= MAX_LINES);
    assert!(s.transcript_bytes <= MAX_TRANSCRIPT_BYTES);
    assert_eq!(s.transcript_bytes, s.lines.iter().map(|line| line.plain().len()).sum::<usize>());
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


#[test]
fn downstream_offer_uses_real_cancel_frame_and_expires_without_sending() {
    let now = Instant::now();
    let mut s = connected();
    s.prepare(submission("work", Some("adu")).unwrap()).unwrap();
    s.apply_at(frame(json!({"type":"error", "code":"cancelled", "message":"Turn cancelled",
        "msg_id":s.msg_id, "conv":s.conv, "downstream_running":2, "downstream_ref":"opaque-never-render"})), now);
    assert!(!s.running);
    assert!(s.status.is(strings::CHAT_CANCELLED));
    assert_eq!(s.downstream_prompt(now).unwrap().plain(), strings::downstream_prompt(2));
    assert!(s.lines.iter().all(|line| !line.plain().contains("opaque-never-render")));
    let expiry = now + DOWNSTREAM_TTL;
    assert!(s.downstream_prompt(expiry).is_none());
    assert!(s.downstream_frame("opaque-never-render", expiry).is_none());
    assert!(s.downstream_frame("foreign", now).is_none());
    let sent = s.downstream_frame("opaque-never-render", now).unwrap();
    assert_eq!(sent["type"], "cancel_downstream");
    assert_eq!(sent["downstream_ref"], "opaque-never-render");
    assert!(sent["request_id"].as_str().is_some());
    assert!(s.downstream_frame("opaque-never-render", now).is_none());
    assert!(s.downstream_prompt(now).is_none());
    s.prepare(submission("new turn", Some("adu")).unwrap()).unwrap();
    s.apply(frame(json!({"type":"downstream_stopped", "request_id":sent["request_id"],
        "stopped":2, "still_stopping":1})));
    assert!(s.running, "stop reply must not end a newer chat turn");
    assert!(s.status.is(&strings::downstream_stopped(2, 1)));
}

#[test]
fn downstream_unknown_and_disconnect_are_plain_and_do_not_stop_a_turn() {
    let now = Instant::now();
    let mut s = connected();
    s.prepare(submission("work", Some("adu")).unwrap()).unwrap();
    s.apply_at(frame(json!({"type":"error", "code":"cancelled", "downstream_running":1,
        "downstream_ref":"opaque"})), now);
    let sent = s.downstream_frame("opaque", now).unwrap();
    s.apply(frame(json!({"type":"error", "code":"downstream_ref_unknown", "request_id":sent["request_id"],
        "message":"hostile message\x1b]52;c;ignored\x07"})));
    assert!(s.status.is(strings::DOWNSTREAM_UNKNOWN));
    assert_eq!(s.lines.back().unwrap().plain(), format!("{}: {}", strings::CHAT_SYSTEM, strings::DOWNSTREAM_UNKNOWN));
    s.prepare(submission("work again", Some("adu")).unwrap()).unwrap();
    s.apply_at(frame(json!({"type":"error", "code":"cancelled", "downstream_running":1,
        "downstream_ref":"new-opaque"})), now);
    s.disconnected();
    assert!(s.downstream_prompt(now).is_none());
    assert!(s.downstream_frame("new-opaque", now).is_none());
}

#[test]
fn downstream_counts_are_numeric_clamped_and_never_terminal_text() {
    let now = Instant::now();
    for (value, expected) in [
        (json!(3), 3), (json!(u64::MAX), MAX_DOWNSTREAM_COUNT), (json!(1e100), MAX_DOWNSTREAM_COUNT),
        (json!(-1), 0), (json!("3"), 0), (json!("\x1b]52;c;hostile\x07"), 0),
        (json!({"count":5}), 0), (json!(null), 0),
    ] {
        let mut s = connected();
        s.prepare(submission("work", Some("adu")).unwrap()).unwrap();
        s.apply_at(frame(json!({"type":"error", "code":"cancelled", "downstream_running":value,
            "downstream_ref":"opaque-never-render"})), now);
        assert_eq!(s.downstream_prompt(now).map(|text| text.plain()),
            (expected > 0).then(|| strings::downstream_prompt(expected)));
        assert!(!s.status.plain().contains("hostile"));
    }
}

#[test]
fn downstream_request_error_does_not_end_new_chat() {
    let now = Instant::now();
    let mut s = connected();
    s.prepare(submission("work", Some("adu")).unwrap()).unwrap();
    s.apply_at(frame(json!({"type":"error", "code":"cancelled", "downstream_running":1,
        "downstream_ref":"opaque"})), now);
    let sent = s.downstream_frame("opaque", now).unwrap();
    s.prepare(submission("new work", Some("adu")).unwrap()).unwrap();
    s.apply(frame(json!({"type":"error", "code":"credential_check_unavailable",
        "request_id":sent["request_id"], "message":"Retry later"})));
    assert!(s.running);
    assert!(s.status.is("Retry later"));
}

#[test]
fn downstream_replies_require_exact_present_request_id_and_busy_preserves_pending() {
    for reply in [json!({"type":"downstream_stopped","stopped":1}),
        json!({"type":"error","code":"downstream_ref_unknown"})] {
        let mut s = connected();
        s.downstream_request = Some("outstanding".into());
        let revision = s.revision;
        s.apply(frame(reply.clone()));
        assert_eq!(s.revision, revision);
        assert_eq!(s.downstream_request.as_deref(), Some("outstanding"));
        let mut wrong = reply.clone();
        wrong["request_id"] = json!("other");
        s.apply(frame(wrong));
        assert_eq!(s.revision, revision);
        s.apply(frame(json!({"type":"error","code":"downstream_stop_busy","request_id":"outstanding","retryable":true})));
        assert_eq!(s.downstream_request.as_deref(), Some("outstanding"));
        let mut matching = reply;
        matching["request_id"] = json!("outstanding");
        s.apply(frame(matching));
        assert!(s.downstream_request.is_none());
    }
}

#[test]
fn downstream_busy_requires_matching_outstanding_request_id() {
    let busy = |request_id: Option<&str>| {
        let mut value = json!({"type":"error","code":"downstream_stop_busy","message":"Still busy"});
        if let Some(request_id) = request_id { value["request_id"] = json!(request_id); }
        frame(value)
    };
    let mut s = connected();
    s.downstream_request = Some("outstanding".into());
    let revision = s.revision;
    s.apply(busy(None));
    assert_eq!(s.revision, revision);
    assert_eq!(s.downstream_request.as_deref(), Some("outstanding"));
    s.apply(busy(Some("wrong")));
    assert_eq!(s.revision, revision);
    assert_eq!(s.downstream_request.as_deref(), Some("outstanding"));
    s.apply(busy(Some("outstanding")));
    assert_eq!(s.revision, revision + 1);
    assert_eq!(s.downstream_request.as_deref(), Some("outstanding"));
    assert_eq!(s.lines.back().unwrap().plain(), "系統: Still busy");

    let mut no_request = connected();
    let revision = no_request.revision;
    no_request.apply(busy(Some("unexpected")));
    assert_eq!(no_request.revision, revision);
    assert!(no_request.downstream_request.is_none());
}

#[test]
fn stale_downstream_busy_preserves_running_turn_and_status() {
    let busy = |request_id: Option<&str>| {
        let mut value = json!({"type":"error","code":"downstream_stop_busy","message":"Stale busy"});
        if let Some(request_id) = request_id { value["request_id"] = json!(request_id); }
        frame(value)
    };

    for (outstanding, request_id) in [
        (Some("outstanding"), None),
        (Some("outstanding"), Some("wrong")),
        (None, Some("unexpected")),
    ] {
        let mut s = connected();
        s.prepare(submission("new turn", Some("adu")).unwrap()).unwrap();
        s.downstream_request = outstanding.map(str::to_owned);
        let status = s.status.clone();
        let line_count = s.lines.len();
        s.apply(busy(request_id));
        assert!(s.running);
        assert_eq!(s.status, status);
        assert_eq!(s.downstream_request.as_deref(), outstanding);
        assert_eq!(s.lines.len(), line_count);
    }
}
