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
    let key = db.member_key_create(&user, "resume", 1, epoch, false, &[]).unwrap();
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
    done(&mut socket, "own", "own-reply", &own).await;
    fixture.turn_count(0).await;
    socket.close(None).await.unwrap();
}
