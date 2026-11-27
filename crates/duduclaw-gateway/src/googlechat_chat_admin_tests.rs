//! RM-143 P14 adapter tests. Intentionally unrun until gateway review.
use super::*;
use crate::approval::{ApprovalId, ApprovalStatus, decision_service::tests::Fixture};
use crate::approval::chat_admin::press::PressOutcome;
use crate::decision_action::{DecisionAct, DecisionSource, encode};
use serde_json::{Value, json};

fn click(id: &str) -> Value {
    let action = encode(DecisionSource::Approval, DecisionAct::Approve, id);
    json!({
        "type":"CARD_CLICKED", "user":{"name":"users/111","type":"HUMAN"},
        "space":{"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE","singleUserBotDm":true},
        "message":{"name":"spaces/DM1/messages/m1", "space":{"name":"spaces/DM1"},
            "sender":{"name":"users/app","type":"BOT"}, "text":"UNTRUSTED-EVENT-TEXT"},
        "common":{"invokedFunction":crate::channel_format::GOOGLECHAT_DECISION_FUNCTION,
            "parameters":{"decision":action}},
        "action":{"actionMethodName":crate::channel_format::GOOGLECHAT_DECISION_FUNCTION,
            "parameters":[{"key":crate::channel_format::GOOGLECHAT_DECISION_PARAM,"value":action}]}
    })
}

async fn fixture() -> (Fixture, ApprovalId, Arc<GoogleChatState>) {
    let f = Fixture::new();
    f.config(true, "googlechat", &[]);
    let id = f.simple(crate::expert_skills::SKILLS_ACTION_KIND).await;
    let proof = f.proof(&id).await;
    // The expiry branch rewrites a receipted card before live identity checks.
    // No Google credential or network is needed for group/missing-ID refusal.
    rusqlite::Connection::open(f.env.home.join("approvals.db")).unwrap().execute(
        "UPDATE chat_admin_cards SET channel='googlechat', account='googlechat',
         principal='users/111', dm='spaces/DM1', message_id='spaces/DM1/messages/m1',
         valid_until='2000-01-01T00:00:00Z' WHERE card_id=?1", [&proof.card_id],
    ).unwrap();
    let ctx = Arc::new(crate::channel_reply::access_gate_tests::reply_ctx(&f.env.home));
    let email = format!("p14-{}@example.invalid", uuid::Uuid::new_v4());
    let creds = GoogleChatCreds::candidate(&json!({"client_email":email,"private_key":"not-a-key"}).to_string()).unwrap();
    let state = Arc::new(GoogleChatState { ctx, creds:Arc::new(creds), agent:None });
    (f, id, state)
}

async fn assert_pending(f: &Fixture, id: &ApprovalId) {
    assert_eq!(f.status(id).await, ApprovalStatus::Pending);
    let count: i64 = rusqlite::Connection::open(f.env.home.join("approvals.db")).unwrap().query_row(
        "SELECT count(*) FROM chat_admin_decisions WHERE approval_id=?1", [id.as_str()], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0);
}

async fn receipt_status(f: &Fixture) -> String {
    rusqlite::Connection::open(f.env.home.join("approvals.db")).unwrap().query_row(
        "SELECT status FROM chat_admin_cards WHERE channel='googlechat' AND account='googlechat'
         AND dm='spaces/DM1' AND message_id='spaces/DM1/messages/m1'", [], |row| row.get(0),
    ).unwrap()
}

async fn response_json(response: axum::response::Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 16384).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn approval_source_accepted_but_function_and_parameters_remain_strict() {
    let event = click("12345678-1234-4234-8234-123456789abc");
    let press = parse_card_press(&event).unwrap();
    assert_eq!(press.user, "users/111");
    assert_eq!(crate::decision_action::parse(&press.action).unwrap().source, DecisionSource::Approval);
    for pointer in ["/common/invokedFunction", "/action/actionMethodName"] {
        for bad in [json!("unknown"), json!("duduclaw_decision_extra")] {
            let mut event = event.clone();
            *event.pointer_mut(pointer).unwrap() = bad;
            assert!(parse_card_press(&event).is_none());
        }
    }
    for bad in ["garbage", "duduclaw:decide:apv:retry:id", "duduclaw:decide:apv:ok:"] {
        let mut event = event.clone();
        event["common"]["parameters"]["decision"] = json!(bad);
        event["action"]["parameters"][0]["value"] = json!(bad);
        assert!(parse_card_press(&event).is_none());
    }
    let mut event = event;
    event["action"]["parameters"].as_array_mut().unwrap().push(
        json!({"key":"decision","value":press.action}));
    assert!(parse_card_press(&event).is_none());
}

#[tokio::test]
async fn group_press_refused() {
    let (f, id, state) = fixture().await;
    for kind in ["SPACE", "GROUP_CHAT"] {
        let mut event = click(id.as_str());
        event["space"]["spaceType"] = json!(kind);
        event["space"]["singleUserBotDm"] = json!(false);
        let press = parse_card_press(&event).unwrap();
        let outcome = route_card_press(&state, &event, &press).await.unwrap();
        assert_eq!(outcome.refused_reason, Some("not_private"));
        assert!(outcome.card_text.is_none());
        let body = response_json(card_clicked(&state, &event).await).await;
        assert_eq!(body, json!({"text":outcome.reply}));
        assert!(!body.to_string().contains("UNTRUSTED-EVENT-TEXT"));
        assert_eq!(receipt_status(&f).await, "live");
        assert_pending(&f, &id).await;
    }
}

#[tokio::test]
async fn foreign_space_or_malformed_message_name_is_none_and_refused() {
    let (f, id, state) = fixture().await;
    let valid = click(id.as_str());
    let press = parse_card_press(&valid).unwrap();
    let origin = card_press_origin(&state, &valid, &press, true);
    assert_eq!(origin.message_id.as_deref(), Some("spaces/DM1/messages/m1"));
    assert_eq!(origin.account, "googlechat");
    assert_eq!(origin.dm, "spaces/DM1");
    for name in [Value::Null, json!("spaces/OTHER/messages/m1"), json!("spaces/DM1/messages/../m1"),
        json!("spaces/DM1/messages/m1/extra"), json!("spaces/DM1/messages/"), json!(42)] {
        let mut event = valid.clone();
        event["message"]["name"] = name;
        // Both space objects still agree: only the message resource is wrong.
        let press = parse_card_press(&event).unwrap();
        assert_eq!(card_press_origin(&state, &event, &press, true).message_id, None);
        let outcome = route_card_press(&state, &event, &press).await.unwrap();
        assert_eq!(outcome.refused_reason, Some("message_id_missing"));
        assert!(outcome.card_text.is_none());
        assert_eq!(receipt_status(&f).await, "live");
        assert_pending(&f, &id).await;
    }
}

#[tokio::test]
async fn expired_card_returns_synchronous_update_with_empty_cards() {
    let (f, id, state) = fixture().await;
    let event = click(id.as_str());
    let body = response_json(card_clicked(&state, &event).await).await;
    assert_eq!(body["actionResponse"]["type"], "UPDATE_MESSAGE");
    assert_eq!(body["cardsV2"], json!([]));
    assert!(body["text"].as_str().unwrap().starts_with("已失效"));
    assert!(body.get("privateMessageViewer").is_none());
    assert!(!body.to_string().contains("UNTRUSTED-EVENT-TEXT"));
    assert_eq!(receipt_status(&f).await, "expired");
    assert_pending(&f, &id).await;
}

#[tokio::test]
async fn non_bot_message_cannot_decide_or_request_an_update() {
    let (f, id, state) = fixture().await;
    for kind in [Value::Null, json!("HUMAN"), json!("UNKNOWN")] {
        let mut event = click(id.as_str());
        event["message"]["sender"]["type"] = kind;
        let press = parse_card_press(&event).unwrap();
        let outcome = route_card_press(&state, &event, &press).await.unwrap();
        assert_eq!(outcome.refused_reason, Some("message_not_bot"));
        assert!(outcome.card_text.is_none());
        assert_eq!(receipt_status(&f).await, "live");
        assert_pending(&f, &id).await;
    }
}

#[test]
fn response_uses_card_text_verbatim_or_space_reply() {
    let press = parse_card_press(&click("12345678-1234-4234-8234-123456789abc")).unwrap();
    let outcome = PressOutcome { reply:"toast".into(), card_text:Some("✓ 已核准：技能（Ada）".into()), card_link:None, refused_reason:None };
    assert_eq!(card_press_response(&press, &outcome), json!({
        "actionResponse":{"type":"UPDATE_MESSAGE"}, "text":outcome.card_text.unwrap(), "cardsV2":[]
    }));
    let outcome = PressOutcome::refused("refused".into(), "not_private", false);
    assert_eq!(card_press_response(&press, &outcome),
        json!({"text":"refused"}));
    let legacy = PressOutcome::from_result(Err("legacy refusal".into()));
    assert_eq!(card_press_response(&press, &legacy), json!({"text":"⚠️ legacy refusal"}));
}

#[tokio::test]
async fn forged_card_click_rejected() {
    use crate::agent_channels::e2e_gchat_tests::{chat_jwt_with, service_account, start_fake};
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    let (f, id, state) = fixture().await;
    let fake = start_fake().await;
    let email = state.creds.client_email();
    // Reuse the fake JWKS and key generation used by the Google Chat HTTP tests.
    let _service_account = service_account(&fake, email);
    let path = f.env.home.join("config.toml");
    let config = std::fs::read_to_string(&path).unwrap().replace("[channels]\n",
        "[channels]\ngooglechat_project_number='143014'\n");
    std::fs::write(path, config).unwrap();
    let app = Router::new().route("/webhook/googlechat", axum::routing::post(webhook_handler))
        .with_state(state.clone());
    let event = click(id.as_str());
    let now = chrono::Utc::now().timestamp();
    let issuer = "chat@system.gserviceaccount.com";
    let tokens = [
        chat_jwt_with(issuer, "143014", now + 600, false), // signature from an absent key, same kid
        chat_jwt_with(issuer, "wrong-project", now + 600, true),
        chat_jwt_with("wrong-issuer", "143014", now + 600, true),
        chat_jwt_with(issuer, "143014", now - 3600, true), // beyond 300s leeway
    ];
    for token in tokens {
        let response = app.clone().oneshot(Request::builder().method("POST").uri("/webhook/googlechat")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(event.to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // If press handling were reached, the expired receipt would be retired.
        assert_eq!(receipt_status(&f).await, "live");
        assert_pending(&f, &id).await;
    }
    // Positive control: the exact same HTTP body reaches press with a valid JWT.
    let token = chat_jwt_with(issuer, "143014", now + 600, true);
    let response = app.oneshot(Request::builder().method("POST").uri("/webhook/googlechat")
        .header("content-type", "application/json").header("authorization", format!("Bearer {token}"))
        .body(Body::from(event.to_string())).unwrap()).await.unwrap();
    let body = response_json(response).await;
    assert_eq!(body["actionResponse"]["type"], "UPDATE_MESSAGE");
    assert_eq!(receipt_status(&f).await, "expired");
    assert_pending(&f, &id).await;
}

#[tokio::test]
async fn pubsub_never_handshakes_or_decides() {
    use duduclaw_auth::{UserRole, chat_link};
    let (f, id, state) = fixture().await;
    let admin = f.db().create_user("pubsub@example.test", "Admin", "fixture-password", UserRole::Admin).unwrap();
    let handshake = chat_link::start(f.db(), &admin.id, "googlechat", "session").unwrap();
    let mut event = click(id.as_str());
    // Even a complete usable approval event and a live handshake code stay inert.
    event["message"]["text"] = json!(handshake.code);
    event["message"]["sender"] = json!({"name":"users/111","type":"HUMAN"});
    assert!(pubsub_control_message(&event));
    process_pubsub_event(&state, &event).await;
    event["message"]["sender"]["type"] = json!("BOT");
    process_pubsub_event(&state, &event).await;
    assert_eq!(receipt_status(&f).await, "live");
    assert_pending(&f, &id).await;
    let status = chat_link::status(f.db(), &admin.id, &handshake.handshake_id).unwrap().unwrap();
    assert_eq!(status.status, chat_link::HandshakeStatus::Waiting);
    assert!(f.db().channel_identities_for_user(&admin.id).unwrap().is_empty());
}

#[test]
fn step_up_response_has_open_link_not_callback_authority() {
    let press = parse_card_press(&click("12345678-1234-4234-8234-123456789abc")).unwrap();
    let url = format!("https://dashboard.example.test/approvals/confirm#ticket={}", "a".repeat(64));
    let outcome = PressOutcome { reply:"需要再確認一次".into(), card_text:Some("需要再確認一次".into()),
        card_link:Some(crate::approval::chat_admin::press::CardLink { label:"前往確認".into(), url:url.clone() }),
        refused_reason:Some("step_up_required") };
    let value = card_press_response(&press, &outcome);
    assert_eq!(value["actionResponse"]["type"], "UPDATE_MESSAGE");
    assert_eq!(value["text"], "需要再確認一次");
    let button = &value["cardsV2"][0]["card"]["sections"][0]["widgets"][0]["buttonList"]["buttons"][0];
    assert_eq!(button["onClick"], json!({"openLink":{"url":url}}));
}
