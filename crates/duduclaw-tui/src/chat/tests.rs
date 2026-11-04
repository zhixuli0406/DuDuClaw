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

#[test]
fn rooms_route_interleaved_frames_and_reject_unknown_or_conflicting_identity() {
    let mut rooms = ChatRooms::default();
    rooms.apply(frame(json!({"type":"session_info","session_id":"base"})));
    let a = rooms.room("pane-a").unwrap().prepare(submission("one", Some("adu")).unwrap()).unwrap();
    let b = rooms.room("pane-b").unwrap().prepare(submission("two", Some("worker")).unwrap()).unwrap();
    for value in [
        json!({"type":"progress","conv":a.conv,"msg_id":a.msg_id,"content":"A working"}),
        json!({"type":"progress","conv":b.conv,"msg_id":b.msg_id,"content":"B working"}),
        json!({"type":"assistant_done","conv":"unknown","content":"foreign"}),
        json!({"type":"assistant_done","conv":a.conv,"msg_id":b.msg_id,"content":"conflict"}),
        json!({"type":"assistant_done","session_id":"unknown","conv":a.conv,"content":"foreign session"}),
        json!({"type":"assistant_done","content":"uncorrelated"}),
    ] { rooms.apply(frame(value)); }
    assert!(rooms.rooms["pane-a"].status.is("A working"));
    assert!(rooms.rooms["pane-b"].status.is("B working"));
    let cancel = rooms.rooms["pane-a"].cancel_frame().unwrap();
    assert_eq!(cancel["msg_id"], a.msg_id);
    rooms.apply(frame(json!({"type":"error","code":"cancelled","conv":a.conv,"msg_id":a.msg_id,
        "downstream_running":2,"downstream_ref":"opaque-a"})));
    assert!(!rooms.rooms["pane-a"].running);
    assert!(rooms.rooms["pane-b"].running);
    assert!(rooms.rooms["pane-a"].downstream_prompt(Instant::now()).is_some());
    assert!(rooms.rooms["pane-b"].downstream_prompt(Instant::now()).is_none());
    rooms.apply(frame(json!({"type":"error","code":"turn_limit","conv":b.conv,"msg_id":b.msg_id})));
    assert!(rooms.rooms["pane-b"].status.is(strings::CHAT_TURN_LIMIT));
    for id in ["pane-c", "pane-d"] { assert!(rooms.room(id).is_some()); }
    assert!(rooms.room("pane-e").is_none());
    rooms.retain(&["pane-b".into()]);
    rooms.apply(frame(json!({"type":"assistant_done","conv":a.conv,"msg_id":a.msg_id,"content":"closed"})));
    assert_eq!(rooms.rooms.len(), 1);
}

#[test]
fn rooms_admission_errors_end_only_the_matching_turn_on_new_and_old_gateways() {
    for code in ["turn_limit", "duplicate_message"] {
        for legacy in [false, true] {
            let mut rooms = ChatRooms::default();
            rooms.apply(frame(json!({"type":"session_info","session_id":"base"})));
            let a = rooms.room("pane-a").unwrap().prepare(submission("one", Some("adu")).unwrap()).unwrap();
            rooms.room("pane-b").unwrap().prepare(submission("two", Some("worker")).unwrap()).unwrap();
            // Exact production admission frame, including the old gateway shape.
            let mut error = json!({"type":"error", "code":code,
                "message":"Turn admission limit reached or duplicate message id",
                "request_id":a.msg_id});
            if !legacy {
                error["session_id"] = json!(format!("base#agent:adu#conv:{}", a.conv));
                error["conv"] = json!(a.conv);
                error["msg_id"] = json!(a.msg_id);
            }
            rooms.apply(frame(error));
            assert!(!rooms.rooms["pane-a"].running, "{code}, legacy={legacy}");
            assert!(rooms.rooms["pane-b"].running);
            let expected = if code == "turn_limit" { strings::CHAT_TURN_LIMIT }
                else { "Turn admission limit reached or duplicate message id" };
            assert!(rooms.rooms["pane-a"].status.is(expected));
            let retry = rooms.room("pane-a").unwrap().prepare(submission("retry", Some("adu")).unwrap()).unwrap();
            assert!(retry.session_id.is_none(), "a rejected first turn must not become a resume request");
        }
    }
}

#[test]
fn rooms_reject_conflicting_stale_and_ambiguous_admission_requests() {
    let mut rooms = ChatRooms::default();
    rooms.apply(frame(json!({"type":"session_info","session_id":"base"})));
    let a = rooms.room("pane-a").unwrap().prepare(submission("one", Some("adu")).unwrap()).unwrap();
    let b = rooms.room("pane-b").unwrap().prepare(submission("two", Some("worker")).unwrap()).unwrap();
    for identity in [
        json!({"request_id":a.msg_id, "conv":b.conv}),
        json!({"request_id":a.msg_id, "msg_id":b.msg_id}),
        json!({"request_id":a.msg_id, "session_id":format!("base#agent:worker#conv:{}", b.conv)}),
        json!({"request_id":"stale", "conv":a.conv, "msg_id":a.msg_id}),
    ] {
        let mut error = identity;
        error["type"] = json!("error");
        error["code"] = json!("turn_limit");
        rooms.apply(frame(error));
        assert!(rooms.rooms["pane-a"].running);
        assert!(rooms.rooms["pane-b"].running);
    }
    rooms.room("pane-b").unwrap().msg_id = Some(a.msg_id.clone());
    rooms.apply(frame(json!({"type":"error", "code":"duplicate_message", "request_id":a.msg_id})));
    assert!(rooms.rooms["pane-a"].running);
    assert!(rooms.rooms["pane-b"].running);
}

#[test]
fn rooms_handshake_sets_connected_status_for_new_existing_and_reconnected_rooms() {
    let mut rooms = ChatRooms::default();
    rooms.apply(frame(json!({"type":"session_info","session_id":"base"})));
    let room = rooms.room("after").unwrap();
    assert!(room.connected);
    assert!(room.status.is(strings::CONNECTED));

    let mut rooms = ChatRooms::default();
    let before = rooms.room("before").unwrap();
    assert!(before.status.is(strings::CONNECTING));
    rooms.apply(frame(json!({"type":"session_info","session_id":"base"})));
    assert!(rooms.rooms["before"].connected);
    assert!(rooms.rooms["before"].status.is(strings::CONNECTED));

    rooms.disconnected();
    rooms.room("reconnected").unwrap();
    rooms.rooms.get_mut("reconnected").unwrap().running = true;
    rooms.apply(frame(json!({"type":"session_info","session_id":"base-2"})));
    assert!(rooms.rooms["reconnected"].connected);
    assert!(rooms.rooms["reconnected"].running);
    assert!(rooms.rooms["reconnected"].status.is(strings::CONNECTING));
}

#[test]
fn rooms_cancel_outcome_can_use_a_server_generated_request_id() {
    let mut rooms = ChatRooms::default();
    rooms.apply(frame(json!({"type":"session_info","session_id":"base"})));
    let a = rooms.room("pane-a").unwrap().prepare(submission("one", Some("adu")).unwrap()).unwrap();
    rooms.apply(frame(json!({"type":"error", "code":"cancelled", "request_id":"server-generated-cancel",
        "session_id":format!("base#agent:adu#conv:{}", a.conv), "conv":a.conv, "msg_id":a.msg_id})));
    assert!(!rooms.rooms["pane-a"].running);
    assert!(rooms.rooms["pane-a"].status.is(strings::CHAT_CANCELLED));
}

#[test]
fn presentation_messages_bound_clean_bytes_and_physical_lines() {
    let mut state = ChatState::default();
    for _ in 0..20 { state.append("員工", &"\x1b\u{202e}".repeat(40_000)); }
    assert!(state.messages.iter().all(|message| message.content.visible().is_empty()));
    state.append("員工", &"\n".repeat(4_000));
    assert!(state.message_lines <= MAX_LINES);
    assert!(state.messages.len() <= MAX_LINES);
    assert!(state.message_bytes <= MAX_TRANSCRIPT_BYTES);
    state.append("員工", &"界".repeat(90_000));
    assert!(state.message_lines <= MAX_LINES);
    assert!(state.message_bytes <= MAX_TRANSCRIPT_BYTES);
}

#[test]
fn oversized_four_byte_reply_keeps_tail_instead_of_evicting_itself() {
    let mut state = ChatState::default();
    state.append("員工", &format!("{}TAIL", "😀".repeat(65_536)));
    assert_eq!(state.messages.len(), 1);
    assert!(state.messages[0].content.plain().ends_with("TAIL"));
    assert!(state.message_bytes <= MAX_TRANSCRIPT_BYTES);
    assert!(state.message_lines <= MAX_LINES);
}
