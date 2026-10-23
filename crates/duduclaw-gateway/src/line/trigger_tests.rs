//! LINE `@` routing: mention-only semantics, one-shot addressee and route
//! snapshot stability.

use super::*;
use crate::trigger_route::tests::write_agent;

async fn fixture(public_sales: bool) -> (tempfile::TempDir, LineState) {
    let dir = tempfile::tempdir().unwrap();
    let agents = dir.path().join("agents");
    write_agent(&agents, "dudu", "嘟嘟總機", "@嘟嘟總機", "main", "");
    write_agent(
        &agents,
        "finance",
        "財務小幫手",
        "@財務小幫手",
        "specialist",
        "",
    );
    let extra = if public_sales {
        "\n[channels]\npublic_trigger = true\n"
    } else {
        ""
    };
    write_agent(
        &agents,
        "sales-rep",
        "業務專員",
        "@sales-rep",
        "specialist",
        extra,
    );
    std::fs::write(
        dir.path().join("config.toml"),
        "[channels]\nline_channel_token='test-access'\nline_channel_secret='test-secret'\n",
    )
    .unwrap();
    let mut registry = duduclaw_agent::AgentRegistry::new(agents);
    registry.scan().await.unwrap();
    let sessions =
        Arc::new(crate::session::SessionManager::new(&dir.path().join("sessions.db")).unwrap());
    let (event_tx, _) = tokio::sync::broadcast::channel(16);
    let channel_status = Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let ctx = Arc::new(ReplyContext::new(
        Arc::new(tokio::sync::RwLock::new(registry)),
        dir.path().to_path_buf(),
        sessions,
        channel_status.clone(),
        event_tx.clone(),
    ));
    let state = LineState {
        home_dir: dir.path().to_path_buf(),
        ctx,
        http: reqwest::Client::new(),
        channel_status,
        event_tx,
        ingress: None,
    };
    (dir, state)
}

fn group_text(id: &str, group: &str, user: &str, text: &str, ts: i64) -> LineEvent {
    serde_json::from_value(serde_json::json!({
        "type": "message",
        "webhookEventId": id,
        "replyToken": "rt",
        "timestamp": ts,
        "source": {"type": "group", "groupId": group, "userId": user},
        "message": {"type": "text", "id": format!("m-{id}"), "text": text}
    }))
    .unwrap()
}

/// The first snapshot of an event (resolves the one-shot addressee).
async fn admit(state: &LineState, ev: &LineEvent) -> LineRoute {
    line_route(state, ev, RouteMode::Admission, true)
        .await
        .unwrap()
}

/// A later recomputation reusing what admission stored.
async fn replay(state: &LineState, ev: &LineEvent, stored: &StoredRoute) -> LineRoute {
    line_route(state, ev, RouteMode::Replay(Some(stored)), true)
        .await
        .unwrap()
}

fn with_mention(
    mut ev: serde_json::Value,
    index: usize,
    length: usize,
    is_self: bool,
) -> LineEvent {
    ev["message"]["mention"] = serde_json::json!({
        "mentionees": [{"index": index, "length": length, "type": "user", "isSelf": is_self}]
    });
    serde_json::from_value(ev).unwrap()
}

fn raw_group_text(id: &str, group: &str, user: &str, text: &str, ts: i64) -> serde_json::Value {
    serde_json::json!({
        "type": "message",
        "webhookEventId": id,
        "replyToken": "rt",
        "timestamp": ts,
        "source": {"type": "group", "groupId": group, "userId": user},
        "message": {"type": "text", "id": format!("m-{id}"), "text": text}
    })
}

#[tokio::test]
async fn mention_only_group_answers_only_messages_that_call_an_employee() {
    let (_d, state) = fixture(false).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    let plain = admit(&state, &group_text("e1", &g, "U1", "大家早安", 1)).await;
    assert!(
        !plain.addresses_bot(),
        "ordinary group chatter stays ignored"
    );
    assert_eq!(plain.agent(), "dudu");

    let call_default = admit(&state, &group_text("e2", &g, "U1", "@嘟嘟總機 你好", 2)).await;
    assert!(call_default.addresses_bot());
    assert_eq!(call_default.agent(), "dudu");
    assert!(
        !call_default.labelled(),
        "the default employee is not labelled"
    );

    // An unmapped LINE friend calling a non-public employee is treated like
    // a name that matches nobody (owner decision on #182 L5): not addressed
    // in a mention-only group, no refusal that would confirm the name, and
    // never a model turn as finance.
    let call_finance = admit(
        &state,
        &group_text("e3", &g, "U1", "@財務小幫手 幫我算帳", 3),
    )
    .await;
    let call_nobody = admit(
        &state,
        &group_text("e3b", &g, "U1", "@沒有這個人 幫我算帳", 3),
    )
    .await;
    assert!(!call_finance.addresses_bot());
    assert_eq!(call_finance.addresses_bot(), call_nobody.addresses_bot());
    assert_eq!(call_finance.decision, TurnDecision::Default);
    assert_eq!(call_finance.agent(), "dudu");

    let list = admit(&state, &group_text("e4", &g, "U1", "@", 4)).await;
    assert!(list.addresses_bot());
    match &list.decision {
        TurnDecision::Reply {
            list: Some(agents), ..
        } => {
            assert_eq!(
                agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
                vec!["dudu"]
            );
        }
        other => panic!("expected the list, got {other:?}"),
    }
}

#[tokio::test]
async fn native_person_mentions_are_blanked_and_a_bot_mention_addresses_it() {
    let (_d, state) = fixture(true).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    // "😀 @業務 午餐？" — a LINE mention of a *person* whose name starts like
    // the employee 業務專員. 😀 is two UTF-16 units.
    let ev = with_mention(
        raw_group_text("n1", &g, "U5", "😀 @業務 午餐？", 1),
        3,
        3,
        false,
    );
    let r = admit(&state, &ev).await;
    assert!(
        !r.addresses_bot(),
        "a person mention never asks for the employee list"
    );
    // The same text without the native mention marker is a partial call.
    let r = admit(&state, &group_text("n2", &g, "U5", "😀 @業務 午餐？", 2)).await;
    assert!(r.addresses_bot());
    // A native mention of the bot itself addresses it (mention only).
    let ev = with_mention(
        raw_group_text("n3", &g, "U5", "@DuDu 今天幾號", 3),
        0,
        5,
        true,
    );
    let r = admit(&state, &ev).await;
    assert!(r.addresses_bot());
    assert_eq!(r.agent(), "dudu");
    // An employee called after a person mention still routes.
    let ev = with_mention(
        raw_group_text("n4", &g, "U5", "@小王 和 @sales-rep 報價", 4),
        0,
        3,
        false,
    );
    let r = admit(&state, &ev).await;
    assert_eq!(r.agent(), "sales-rep");
    assert!(r.labelled());
}

#[tokio::test]
async fn tapped_employee_gets_only_the_next_message_and_replay_is_stable() {
    let (_d, state) = fixture(true).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    // Tapping 業務專員 in the list sends `@業務專員` as the user.
    let bare_ev = group_text("t1", &g, "U2", "@業務專員", 10);
    let bare = admit(&state, &bare_ev).await;
    match &bare.decision {
        TurnDecision::Reply {
            text,
            list: None,
            kind,
        } => {
            assert!(text.contains("業務專員"));
            assert_eq!(*kind, ReplyKind::AskTask);
        }
        other => panic!("expected the task prompt, got {other:?}"),
    }
    // Another member's message is not taken.
    let other = admit(&state, &group_text("t2", &g, "U3", "午餐吃什麼", 11)).await;
    assert!(!other.addresses_bot());

    let next = group_text("t3", &g, "U2", "幫我報價", 12);
    let first = admit(&state, &next).await;
    assert!(
        first.addresses_bot(),
        "the waited-for message passes mention-only"
    );
    assert_eq!(first.agent(), "sales-rep");
    assert!(first.labelled());
    let stored = first.stored();
    assert_eq!(
        stored,
        StoredRoute {
            agent: "sales-rep".into(),
            one_shot: true
        }
    );
    // Replaying the event (before running, before each send, after an
    // operator retry) gives the same employee without touching the store,
    // even after the sender armed someone else meanwhile.
    let _ = admit(&state, &group_text("t5", &g, "U2", "@嘟嘟總機", 14)).await;
    for _ in 0..3 {
        let again = replay(&state, &next, &stored).await;
        assert_eq!(again.agent(), "sales-rep");
        assert!(again.addresses_bot());
    }
    // Replaying the bare-call event never re-arms anything.
    let bare_stored = bare.stored();
    let _ = replay(&state, &bare_ev, &bare_stored).await;
    // The next message goes to the addressee armed at t5 (the default
    // employee), not to a re-armed 業務專員.
    let after = admit(&state, &group_text("t6", &g, "U2", "那上個月呢？", 15)).await;
    assert_eq!(after.agent(), "dudu");
    let after = admit(&state, &group_text("t7", &g, "U2", "再一次", 16)).await;
    assert!(!after.addresses_bot(), "no stickiness after the one-shot");
}

#[tokio::test]
async fn replay_refuses_an_addressee_the_sender_may_no_longer_call() {
    let (_d, state) = fixture(false).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    let ev = group_text("r1", &g, "U9", "報價單", 1);
    // Stored at admission while sales-rep was open; it is no longer.
    let stored = StoredRoute {
        agent: "sales-rep".into(),
        one_shot: true,
    };
    let r = replay(&state, &ev, &stored).await;
    assert!(
        !r.one_shot,
        "the stored addressee is dropped when no longer callable"
    );
}

#[tokio::test]
async fn postback_events_do_not_consume_the_one_shot() {
    let (_d, state) = fixture(true).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    let _ = admit(&state, &group_text("p1", &g, "U4", "@sales-rep", 20)).await;
    let postback: LineEvent = serde_json::from_value(serde_json::json!({
        "type": "postback",
        "webhookEventId": "p2",
        "replyToken": "rt",
        "timestamp": 21,
        "source": {"type": "group", "groupId": g, "userId": "U4"},
        "postback": {"data": "duduclaw:new_session"}
    }))
    .unwrap();
    assert!(!admit(&state, &postback).await.addresses_bot());
    let next = admit(&state, &group_text("p3", &g, "U4", "報價單", 22)).await;
    assert_eq!(next.agent(), "sales-rep");
}

#[test]
fn direct_reply_renders_the_list_as_quick_replies() {
    let agents = vec![crate::trigger_route::list::CallableAgent {
        id: "dudu".into(),
        display_name: "嘟嘟總機".into(),
        icon: String::new(),
        call_text: "@嘟嘟總機".into(),
    }];
    let lang = crate::trigger_route::texts::Lang::ZhTw;
    let msgs = direct_reply_messages("ignored", Some(&agents), lang);
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0]["quickReply"]["items"][0]["action"]["text"],
        "@嘟嘟總機"
    );
    let msgs = direct_reply_messages("拒絕", None, lang);
    assert_eq!(msgs[0]["text"], "拒絕");
    assert!(msgs[0].get("quickReply").is_none());
}
