//! Identity refusal keeps correlation without ever delivering reply content.
use super::*;
use crate::handlers::live_context::{SocketRecovery, SocketValidity};

async fn write_frame(frame: Outbound, validity: SocketValidity) -> Vec<Message> {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let captured = sent.clone();
    let accepting = futures_util::sink::unfold(captured, |captured, message: Message| async {
        captured.lock().unwrap().push(message);
        Ok::<_, ()>(captured)
    });
    let mut writer = BoundedSocketSink(Box::pin(accepting));
    let _ = frame.deliver_authorized(validity, &mut SocketRecovery::default(), &mut writer).await;
    let frames = sent.lock().unwrap().clone();
    frames
}

fn notice(frame: &Message, admission: bool) {
    let Message::Text(text) = frame else { panic!("expected refusal") };
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(value, serde_json::json!({
        "type": "error", "code": "identity_unavailable",
        "message": if admission { "暫時無法確認登入狀態，這則訊息沒有送出，請稍後再試。" }
            else { "暫時無法確認登入狀態，無法交付回覆，請稍後再試。" },
        "msg_id": "m", "request_id": "m", "conv": "c", "session_id": "session",
    }));
}

#[tokio::test]
async fn pre_admission_notice_survives_unavailable_but_never_revoked_writer() {
    for writer_validity in [SocketValidity::Unavailable, SocketValidity::Valid, SocketValidity::Revoked] {
        let (tx, mut rx) = outbound_channel();
        let sink = TurnSink::new(tx, "session", Some("c".into()), "m");
        // Same refusal as the post-routing, pre-admission credential recheck.
        sink.identity_unavailable(SocketValidity::Unavailable, true).await;
        let sent = write_frame(rx.recv().await.unwrap(), writer_validity).await;
        if writer_validity == SocketValidity::Revoked {
            assert!(sent.iter().all(|frame| matches!(frame, Message::Close(_))));
        } else {
            assert_eq!(sent.len(), 1);
            notice(&sent[0], true);
        }
    }
    let (tx, mut rx) = outbound_channel();
    let sink = TurnSink::new(tx, "session", Some("c".into()), "m");
    sink.identity_unavailable(SocketValidity::Revoked, true).await;
    drop(sink);
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn writer_refusal_rejects_reply_ack_and_only_emits_fixed_notice() {
    for validity in [SocketValidity::Unavailable, SocketValidity::Revoked] {
        let (tx, mut rx) = outbound_channel();
        let mut sink = TurnSink::new(tx, "session", Some("c".into()), "m").acknowledged();
        let send = tokio::spawn(async move {
            sink.send(Message::Text(serde_json::json!({
                "type": "assistant_done", "content": "SECRET_REPLY", "artifact": {"secret":true},
                "thinking": {"text":"SECRET_THINKING"},
            }).to_string().into())).await
        });
        let frame = rx.recv().await.unwrap();
        let sent = write_frame(frame, validity).await;
        assert!(send.await.unwrap().is_err(), "reply delivery must still fail and roll back");
        if validity == SocketValidity::Unavailable {
            assert_eq!(sent.len(), 1);
            notice(&sent[0], false);
        } else {
            assert!(sent.iter().all(|frame| matches!(frame, Message::Close(_))));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn expired_or_abandoned_reply_has_no_late_identity_notice() {
    for closed in [false, true] {
        let (ack, received) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + SEND_TIMEOUT;
        let notice = IdentityUnavailable::new("session", Some("c".into()), "m", false);
        let frame = Outbound { message: Message::Text("SECRET".into()), ack: Some(ack),
            deadline: Some(deadline), identity_notice: Some(notice) };
        if closed {
            drop(received);
            assert!(write_frame(frame, SocketValidity::Unavailable).await.is_empty());
        } else {
            tokio::time::advance(SEND_TIMEOUT).await;
            assert!(write_frame(frame, SocketValidity::Unavailable).await.is_empty());
            assert!(!received.await.unwrap());
        }
    }
}

#[tokio::test]
async fn closed_recovery_never_reopens_for_identity_notice() {
    let (tx, mut rx) = outbound_channel();
    let sink = TurnSink::new(tx, "session", Some("c".into()), "m");
    sink.identity_unavailable(SocketValidity::Unavailable, false).await;
    let frame = rx.recv().await.unwrap();
    let mut recovery = SocketRecovery::default();
    recovery.observe(SocketValidity::Revoked, false);
    let sent = Arc::new(Mutex::new(Vec::new()));
    let captured = sent.clone();
    let accepting = futures_util::sink::unfold(captured, |captured, message: Message| async {
        captured.lock().unwrap().push(message);
        Ok::<_, ()>(captured)
    });
    let mut writer = BoundedSocketSink(Box::pin(accepting));
    assert!(frame.deliver_authorized(SocketValidity::Unavailable, &mut recovery, &mut writer).await.is_err());
    assert!(sent.lock().unwrap().iter().all(|frame| matches!(frame, Message::Close(_))));
}

#[tokio::test]
async fn delivery_checks_refuse_without_content_at_both_stages() {
    for validity in [SocketValidity::Unavailable, SocketValidity::Revoked] {
        for final_check in [false, true] {
            let home = tempfile::tempdir().unwrap();
            crate::trigger_route::tests::write_agent(&home.path().join("agents"), "dudu", "Main", "@dudu", "main", "");
            let state = credential_fixture(home.path()).await;
            let mut gate = crate::agent_channels::e2e_seam::arm_waiting(home.path());
            let ticket = state.turns.admit("alice", "session", Some("c".into()), "m", 3, 10).unwrap();
            let (tx, mut rx) = outbound_channel();
            let task = tokio::spawn(execute(credential_input(state.clone()),
                TurnSink::new(tx, "session", Some("c".into()), "m"), ticket));
            let entered = tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.recv()).await.unwrap().unwrap();
            // No credential watcher for this synthetic widget input: these are
            // precisely run's post-model check and send_terminal's final check.
            if final_check { state.credential_results.lock().unwrap().push_back(SocketValidity::Valid); }
            state.credential_results.lock().unwrap().push_back(validity);
            entered.release.send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), task).await.unwrap().unwrap();
            assert!(state.credential_results.lock().unwrap().is_empty(), "both delivery stages were reached");
            assert!(state.turns.entries.lock().unwrap().is_empty());
            // The model seam does not persist an assistant row. This test
            // checks refusal, not the separate GuardedReply row rollback.
            let mut notices = 0;
            while let Some(frame) = rx.recv().await {
                let Message::Text(text) = &frame.message else { continue; };
                let value: serde_json::Value = serde_json::from_str(text).unwrap();
                assert_ne!(value["type"], "assistant_done");
                if value["code"] == "identity_unavailable" {
                    notices += 1;
                    let sent = write_frame(frame, validity).await;
                    assert_eq!(sent.len(), 1);
                    notice(&sent[0], false);
                }
            }
            assert_eq!(notices, if validity == SocketValidity::Unavailable { 1 } else { 0 });
        }
    }
}

#[tokio::test]
async fn unavailable_and_revoked_remove_only_owned_archives() {
    for validity in [SocketValidity::Unavailable, SocketValidity::Revoked] {
        let archive = tempfile::NamedTempFile::new().unwrap();
        let original = tempfile::NamedTempFile::new().unwrap();
        let mut recovery = SocketRecovery::default();
        assert!(!rollback_before_final_reply(validity, &mut recovery, &[archive.path().into()]).await);
        assert!(!archive.path().exists());
        assert!(original.path().exists());
        assert_eq!(recovery.closed(), validity == SocketValidity::Revoked);
    }
}

#[test]
fn before_routing_notice_has_only_validated_correlation_and_no_session() {
    let longest = "x".repeat(64);
    let too_long = "x".repeat(65);
    for msg_id in [None, Some(""), Some("a b"), Some("a:b"), Some("中文"),
        Some(too_long.as_str()), Some("m-1_A"), Some(longest.as_str())] {
        let frame = IdentityUnavailable::before_routing(msg_id, Some("c-1_A"));
        let Message::Text(text) = frame else { panic!("expected refusal") };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut expected = serde_json::json!({
            "type": "error", "code": "identity_unavailable",
            "message": "暫時無法確認登入狀態，這則訊息沒有送出，請稍後再試。",
            "conv": "c-1_A",
        });
        if let Some(id) = msg_id.filter(|id| *id == "m-1_A" || *id == longest.as_str()) {
            expected["msg_id"] = id.into();
            expected["request_id"] = id.into();
        }
        assert_eq!(value, expected);
        assert!(value.get("session_id").is_none());
    }
}

#[test]
fn before_routing_notice_reuses_bounded_conversation_sanitization() {
    let long = "c".repeat(65);
    for conv in [None, Some(""), Some("::##"), Some("a:b#c/d e"), Some(long.as_str())] {
        let Message::Text(text) = IdentityUnavailable::before_routing(None, conv)
            else { panic!("expected refusal") };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let expected = match conv {
            Some("a:b#c/d e") => Some("abcde".to_owned()),
            Some(id) if id == long => Some("c".repeat(64)),
            _ => None,
        };
        assert_eq!(value.get("conv").and_then(|v| v.as_str()), expected.as_deref());
        assert!(value.get("session_id").is_none());
        assert!(value.get("msg_id").is_none());
        assert!(value.get("request_id").is_none());
    }
}
