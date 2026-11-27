//! Exercise real member-key authentication and the socket resume gate.
use super::*;
use crate::session_origin::{OriginKind, SessionOrigin};

#[tokio::test]
async fn member_key_owner_socket_resumes_only_stored_own_webchat() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let mut fixture = Fixture::start(true).await.unwrap();
    std::fs::write(fixture._home.path().join("config.toml"),
        "[member_api_keys]\nenabled=true\n[people]\nbound_history_privacy=false\n").unwrap();
    let (user, _) = fixture.employee("resume-key", &["finance"], Some("finance")).await;
    let db = &fixture.state.user_db;
    let epoch = db.get_user(&user).unwrap().unwrap().credential_epoch;
    let key = db.member_key_create(&user, "resume", 1, epoch, true, &[]).unwrap();
    let tag = webchat_owner_tag(&user);
    let own = format!("webchat:webchat:{tag}:own");
    let other = format!("webchat:webchat:{}:other", webchat_owner_tag("other-member"));
    // A plausible id cannot override the host-stored origin or owner.
    let wrong_owner = format!("webchat:webchat:{tag}:wrong-owner");
    let visitor = format!("webchat:webchat:{tag}:visitor");
    let unknown = format!("webchat:webchat:{tag}:unknown");
    let missing = format!("webchat:webchat:{tag}:missing");
    let line = "line:C0123456789abcdef0123456789abcdef";
    for (id, origin) in [
        (own.as_str(), SessionOrigin::webchat_user(&user)),
        (other.as_str(), SessionOrigin::webchat_user("other-member")),
        (wrong_owner.as_str(), SessionOrigin::webchat_user("other-member")),
        (visitor.as_str(), SessionOrigin::of_kind(OriginKind::WebchatVisitor)),
        (unknown.as_str(), SessionOrigin::of_kind(OriginKind::Unknown)),
        (line, SessionOrigin::of_kind(OriginKind::Channel)),
    ] {
        fixture.state.ctx.session_manager.get_or_create_with_origin(id, "finance", &origin).await.unwrap();
    }
    let mut socket = fixture.open().await.unwrap();
    send(&mut socket, json!({"type":"auth", "member_key":key.secret})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    for id in [other.as_str(), line, wrong_owner.as_str(), visitor.as_str(), unknown.as_str(), missing.as_str()] {
        send(&mut socket, json!({"type":"user_message", "content":"Hello", "session_id":id,
            "agent":"finance", "msg_id":"refused"})).await;
        let refusal = frame(&mut socket).await;
        assert_eq!(refusal["type"], "error", "{id}: {refusal}");
        assert_eq!(refusal["message"], "conversation not found", "{id}");
        send(&mut socket, json!({"type":"subscribe", "session_id":id, "conv":"refused"})).await;
        assert_eq!(frame(&mut socket).await["type"], "error");
    }
    assert!(fixture.gate.entered.try_recv().is_err(), "refused resume must never dispatch");
    send(&mut socket, json!({"type":"subscribe", "session_id":own, "conv":"own"})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    assert_eq!(frame(&mut socket).await["type"], "subscribed");
    send(&mut socket, json!({"type":"user_message", "content":"Hello", "session_id":own,
        "agent":"finance", "conv":"own", "msg_id":"own-reply"})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    let pending = fixture.entered().await;
    assert_eq!(pending.session_id, own);
    pending.release.send(()).unwrap();
    member_reply_done(&mut socket, "own", "own-reply", &own).await;
    fixture.turn_count(0).await;
    // The same authenticated socket must observe an administrator's narrowing.
    let admin = fixture.state.user_db.create_user("resume-admin@test.invalid", "Admin",
        "isolated-test-password", duduclaw_auth::UserRole::Admin).unwrap();
    fixture.state.user_db.member_key_narrow_admin(&admin.id, &key.info.id, Some(false), None).unwrap();
    for kind in ["user_message", "subscribe"] {
        send(&mut socket, json!({"type":kind, "content":"Repeat history", "session_id":own,
            "agent":"finance", "conv":"narrowed", "msg_id":"narrowed"})).await;
        assert_eq!(frame(&mut socket).await["type"], "error");
    }
    assert!(fixture.gate.entered.try_recv().is_err());
    // Starting a new conversation remains available to this now-unchecked key.
    send(&mut socket, json!({"type":"user_message", "content":"New conversation",
        "agent":"finance", "conv":"new", "msg_id":"new"})).await;
    let pending = fixture.entered().await;
    let new_sid = pending.session_id.clone();
    assert_ne!(new_sid, own);
    pending.release.send(()).unwrap();
    member_reply_done(&mut socket, "new", "new", &new_sid).await;
    fixture.turn_count(0).await;
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn unchecked_member_key_resume_scope_fails_closed() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    let fixture = Fixture::start(true).await.unwrap();
    let home = fixture._home.path();
    std::fs::write(home.join("config.toml"), "[member_api_keys]\nenabled=true\n").unwrap();
    let (user, _) = fixture.employee("unchecked-resume", &["finance"], Some("finance")).await;
    let db = &fixture.state.user_db;
    let epoch = db.get_user(&user).unwrap().unwrap().credential_epoch;
    let key = db.member_key_create(&user, "unchecked", 1, epoch, false, &[]).unwrap();
    let ctx = crate::member_keys::authenticate(home, db, &key.secret).unwrap();
    let credential = ChatCredential::Member(ctx);
    assert!(!fixture.state.member_conversation_allowed(Some(&credential)).await);
    let own = format!("webchat:webchat:{}:unchecked-own", webchat_owner_tag(&user));
    fixture.state.ctx.session_manager.get_or_create_with_origin(&own, "finance",
        &SessionOrigin::webchat_user(&user)).await.unwrap();
    let mut socket = fixture.open().await.unwrap();
    send(&mut socket, json!({"type":"auth", "member_key":key.secret})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    for kind in ["user_message", "subscribe"] {
        send(&mut socket, json!({"type":kind, "content":"Repeat history", "session_id":own,
            "agent":"finance", "conv":"unchecked", "msg_id":"unchecked"})).await;
        let refused = frame(&mut socket).await;
        assert_eq!(refused["type"], "error");
        if kind == "user_message" { assert_eq!(refused["message"], "conversation not found"); }
    }
    socket.close(None).await.unwrap();
    // A fresh checked key also fails closed on unreadable or malformed authority.
    let checked = db.member_key_create(&user, "checked", 1, epoch, true, &[]).unwrap();
    let checked_ctx = crate::member_keys::authenticate(home, db, &checked.secret).unwrap();
    let checked_credential = ChatCredential::Member(checked_ctx);
    assert!(fixture.state.member_conversation_allowed(Some(&checked_credential)).await);
    std::fs::write(home.join("config.toml"), "invalid [").unwrap();
    assert!(!fixture.state.member_conversation_allowed(Some(&checked_credential)).await);
    std::fs::remove_file(home.join("config.toml")).unwrap();
    assert!(!fixture.state.member_conversation_allowed(Some(&checked_credential)).await);
    assert!(fixture.state.member_conversation_allowed(None).await);
}

#[tokio::test]
async fn unchecked_member_key_continues_new_conversation_only_on_creating_socket() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    member_key_new_conversation_roundtrip(false).await;
}

#[tokio::test]
async fn checked_member_key_continues_new_conversation_and_resumes_on_new_socket() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    member_key_new_conversation_roundtrip(true).await;
}

async fn member_key_new_conversation_roundtrip(include_conversations: bool) {
    let mut fixture = Fixture::start(true).await.unwrap();
    std::fs::write(fixture._home.path().join("config.toml"),
        "[member_api_keys]\nenabled=true\n").unwrap();
    let (user, _) = fixture.employee("new-conversation-key", &["finance"], Some("finance")).await;
    let epoch = fixture.state.user_db.get_user(&user).unwrap().unwrap().credential_epoch;
    let key = fixture.state.user_db.member_key_create(&user, "new-conversation", 1, epoch,
        include_conversations, &[]).unwrap();
    let mut socket = fixture.open().await.unwrap();
    send(&mut socket, json!({"type":"auth", "member_key":key.secret})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    let conv = "new-conversation";

    // The second turn uses the same new-or-continue path after the row is persisted.
    send(&mut socket, json!({"type":"user_message", "content":"First message",
        "agent":"finance", "conv":conv, "msg_id":"first"})).await;
    let pending = fixture.entered().await;
    let sid = pending.session_id.clone();
    pending.release.send(()).unwrap();
    member_reply_done(&mut socket, conv, "first", &sid).await;
    fixture.turn_count(0).await;
    assert_eq!(fixture.state.ctx.session_manager.session_agent(&sid).await.unwrap().as_deref(),
        Some("finance"));
    send(&mut socket, json!({"type":"user_message", "content":"Second message",
        "agent":"finance", "conv":conv, "msg_id":"second"})).await;
    let pending = fixture.entered().await;
    assert_eq!(pending.session_id, sid);
    pending.release.send(()).unwrap();
    member_reply_done(&mut socket, conv, "second", &sid).await;
    fixture.turn_count(0).await;

    // Echoing the persisted id exercises the resume-shaped path on its creating socket.
    send(&mut socket, json!({"type":"subscribe", "session_id":sid, "conv":conv})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    let subscribed = frame(&mut socket).await;
    assert_eq!(subscribed["type"], "subscribed");
    assert_eq!(subscribed["session_id"], sid);
    send(&mut socket, json!({"type":"user_message", "content":"Third message", "session_id":sid,
        "agent":"finance", "conv":conv, "msg_id":"third"})).await;
    let pending = fixture.entered().await;
    assert_eq!(pending.session_id, sid);
    pending.release.send(()).unwrap();
    member_reply_done(&mut socket, conv, "third", &sid).await;
    fixture.turn_count(0).await;
    socket.close(None).await.unwrap();

    // Socket-local provenance must not become authority for another connection.
    let mut socket = fixture.open().await.unwrap();
    send(&mut socket, json!({"type":"auth", "member_key":key.secret})).await;
    assert_eq!(frame(&mut socket).await["type"], "session_info");
    send(&mut socket, json!({"type":"subscribe", "session_id":sid, "conv":conv})).await;
    if include_conversations {
        assert_eq!(frame(&mut socket).await["type"], "session_info");
        assert_eq!(frame(&mut socket).await["type"], "subscribed");
    } else {
        assert_eq!(frame(&mut socket).await["type"], "error");
    }
    send(&mut socket, json!({"type":"user_message", "content":"Resume message", "session_id":sid,
        "agent":"finance", "conv":conv, "msg_id":"resume"})).await;
    if include_conversations {
        let pending = fixture.entered().await;
        assert_eq!(pending.session_id, sid);
        pending.release.send(()).unwrap();
        member_reply_done(&mut socket, conv, "resume", &sid).await;
        fixture.turn_count(0).await;
    } else {
        let refused = frame(&mut socket).await;
        assert_eq!(refused["type"], "error");
        assert_eq!(refused["message"], "conversation not found");
        assert!(fixture.gate.entered.try_recv().is_err(), "new socket must not dispatch a resume");
    }
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn member_key_routed_existing_conversation_requires_history_permission() {
    let _serial = SOCKET_TEST_LOCK.lock().await;
    for include_conversations in [false, true] {
        let mut fixture = Fixture::start(true).await.unwrap();
        std::fs::write(fixture._home.path().join("config.toml"),
            "[member_api_keys]\nenabled=true\n").unwrap();
        let (user, _) = fixture.employee("routed-key", &["finance", "sales"], Some("finance")).await;
        let epoch = fixture.state.user_db.get_user(&user).unwrap().unwrap().credential_epoch;
        let key = fixture.state.user_db.member_key_create(&user, "routed", 1, epoch,
            include_conversations, &[]).unwrap();
        let mut socket = fixture.open().await.unwrap();
        send(&mut socket, json!({"type":"auth", "member_key":key.secret})).await;
        let info = frame(&mut socket).await;
        assert_eq!(info["type"], "session_info");
        let base = info["session_id"].as_str().unwrap();
        let conv = "routed-history";
        let selected_sid = compose_session_id(base, Some("finance"), Some(conv));
        let routed_sid = compose_session_id(base, Some("sales"), Some(conv));
        assert!(fixture.state.ctx.session_manager.session_agent(&selected_sid).await.unwrap().is_none());
        fixture.state.ctx.session_manager.get_or_create_with_origin(&routed_sid, "sales",
            &SessionOrigin::webchat_user(&user)).await.unwrap();

        // A new finance bucket must not grant access to the existing sales bucket.
        send(&mut socket, json!({"type":"user_message", "content":"@sales Hello",
            "agent":"finance", "conv":conv, "msg_id":"routed"})).await;
        if include_conversations {
            let pending = fixture.entered().await;
            assert_eq!(pending.session_id, routed_sid);
            pending.release.send(()).unwrap();
            member_reply_done(&mut socket, conv, "routed", &routed_sid).await;
            fixture.turn_count(0).await;
        } else {
            loop {
                let refusal = frame(&mut socket).await;
                if refusal["type"] == "session_info" { continue; }
                assert_eq!(refusal["type"], "error", "{refusal}");
                assert_eq!(refusal["message"], "conversation not found");
                break;
            }
            assert!(fixture.gate.entered.try_recv().is_err(), "routed history must not dispatch");
        }
        assert!(fixture.state.ctx.session_manager.session_agent(&selected_sid).await.unwrap().is_none());
        socket.close(None).await.unwrap();
    }
}

// Non-admin target refreshes may precede the response on both new and resumed turns.
async fn member_reply_done(socket: &mut Socket, conv: &str, msg: &str, sid: &str) {
    loop {
        let result = frame(socket).await;
        if result["type"] == "session_info" { continue; }
        assert_eq!(result["type"], "assistant_done", "{result}");
        assert_eq!(result["conv"], conv);
        assert_eq!(result["msg_id"], msg);
        assert_eq!(result["session_id"], sid);
        assert!(result["content"].as_str().unwrap().contains("OWN_BOT_REPLY"));
        break;
    }
}
