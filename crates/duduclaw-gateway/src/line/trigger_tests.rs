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

#[tokio::test]
async fn mention_only_group_answers_only_messages_that_call_an_employee() {
    let (_d, state) = fixture(false).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    let plain = line_route(&state, &group_text("e1", &g, "U1", "大家早安", 1), true)
        .await
        .unwrap();
    assert!(
        !plain.addresses_bot(),
        "ordinary group chatter stays ignored"
    );
    assert_eq!(plain.agent(), "dudu");

    let call_default = line_route(
        &state,
        &group_text("e2", &g, "U1", "@嘟嘟總機 你好", 2),
        true,
    )
    .await
    .unwrap();
    assert!(call_default.addresses_bot());
    assert_eq!(call_default.agent(), "dudu");
    assert!(
        !call_default.labelled(),
        "the default employee is not labelled"
    );

    // An unmapped LINE friend calling a non-public employee is answered
    // with a refusal (addressed, but no model turn as finance).
    let call_finance = line_route(
        &state,
        &group_text("e3", &g, "U1", "@財務小幫手 幫我算帳", 3),
        true,
    )
    .await
    .unwrap();
    assert!(call_finance.addresses_bot());
    assert!(matches!(call_finance.decision, TurnDecision::Reply { .. }));
    assert_eq!(call_finance.agent(), "dudu");

    let list = line_route(&state, &group_text("e4", &g, "U1", "@", 4), true)
        .await
        .unwrap();
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
async fn tapped_employee_gets_only_the_next_message_and_the_route_is_stable() {
    let (_d, state) = fixture(true).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    // Tapping 業務專員 in the list sends `@業務專員` as the user.
    let bare = line_route(&state, &group_text("t1", &g, "U2", "@業務專員", 10), false)
        .await
        .unwrap();
    match &bare.decision {
        TurnDecision::Reply { text, list: None } => assert!(text.contains("業務專員")),
        other => panic!("expected the task prompt, got {other:?}"),
    }
    // Another member's message is not taken.
    let other = line_route(&state, &group_text("t2", &g, "U3", "午餐吃什麼", 11), true)
        .await
        .unwrap();
    assert!(!other.addresses_bot());

    let next = group_text("t3", &g, "U2", "幫我報價", 12);
    let first = line_route(&state, &next, true).await.unwrap();
    assert!(
        first.addresses_bot(),
        "the waited-for message passes mention-only"
    );
    assert_eq!(first.agent(), "sales-rep");
    assert!(first.labelled());
    // Recomputing the same event (durable ingress admit / pre-send check)
    // gives the same employee.
    for _ in 0..3 {
        assert_eq!(
            line_route(&state, &next, true).await.unwrap().agent(),
            "sales-rep"
        );
    }
    // The message after that is normal routing again (no stickiness).
    let after = line_route(
        &state,
        &group_text("t4", &g, "U2", "那上個月呢？", 13),
        true,
    )
    .await
    .unwrap();
    assert!(!after.addresses_bot());
    assert_eq!(after.agent(), "dudu");
}

#[tokio::test]
async fn postback_events_do_not_consume_the_one_shot() {
    let (_d, state) = fixture(true).await;
    let g = format!("G{}", uuid::Uuid::new_v4().simple());
    let _ = line_route(&state, &group_text("p1", &g, "U4", "@sales-rep", 20), false).await;
    let postback: LineEvent = serde_json::from_value(serde_json::json!({
        "type": "postback",
        "webhookEventId": "p2",
        "replyToken": "rt",
        "timestamp": 21,
        "source": {"type": "group", "groupId": g, "userId": "U4"},
        "postback": {"data": "duduclaw:new_session"}
    }))
    .unwrap();
    assert!(
        !line_route(&state, &postback, true)
            .await
            .unwrap()
            .addresses_bot()
    );
    let next = line_route(&state, &group_text("p3", &g, "U4", "報價單", 22), true)
        .await
        .unwrap();
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
    let msgs = direct_reply_messages("ignored", Some(&agents));
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0]["quickReply"]["items"][0]["action"]["text"],
        "@嘟嘟總機"
    );
    let msgs = direct_reply_messages("拒絕", None);
    assert_eq!(msgs[0]["text"], "拒絕");
    assert!(msgs[0].get("quickReply").is_none());
}
