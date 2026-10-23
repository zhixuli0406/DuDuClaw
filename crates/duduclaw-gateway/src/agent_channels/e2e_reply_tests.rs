//! B11 follow-up (review H1/M1–M3): answers that leave the reply pipeline
//! later — delegated sub-agent answers, reminders, messages sent by a tool,
//! sends with no explicit sender — reach the right chat through the bot / app
//! the conversation came in through.

use std::time::Duration;

use serde_json::json;

use super::e2e_gchat_tests as gchat;
use super::e2e_tests::{FakeTelegramServer, telegram_section};

fn reply_texts(sent: &[(String, serde_json::Value)]) -> Vec<(String, String)> {
    sent.iter()
        .filter(|(m, _)| m == "sendMessage")
        .map(|(_, b)| {
            (
                b["chat_id"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| b["chat_id"].to_string()),
                b["text"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

fn add_callback(
    home: &std::path::Path,
    msg: &str,
    agent: &str,
    ch: &str,
    chat: &str,
    sender: Option<&str>,
) {
    crate::message_queue::MessageQueue::open(home).unwrap();
    let conn = rusqlite::Connection::open(home.join("message_queue.db")).unwrap();
    conn.execute(
        "INSERT INTO delegation_callbacks (message_id, agent_id, channel_type, channel_id, thread_id, created_at, sender_agent) \
         VALUES (?1, ?2, ?3, ?4, NULL, '2026-10-23T00:00:00Z', ?5)",
        rusqlite::params![msg, agent, ch, chat, sender],
    )
    .unwrap();
}

#[test]
fn delivery_addresses_never_carry_the_employee_bucket() {
    assert_eq!(
        crate::chat_commands::source_from_session("telegram:555#agent:sales"),
        ("telegram".to_string(), "555".to_string())
    );
    assert_eq!(
        crate::decision_notify::parse_origin("googlechat:spaces/AAA#agent:sales"),
        Some(("googlechat".to_string(), "spaces/AAA".to_string()))
    );
    assert_eq!(
        crate::decision_notify::parse_origin("telegram:-100:7#agent:hr"),
        Some(("telegram".to_string(), "-100".to_string()))
    );
}

#[tokio::test]
async fn reply_sender_follows_only_the_origin_conversation() {
    let got = crate::claude_runner::REPLY_CHANNEL
        .scope(
            "telegram:555".to_string(),
            crate::claude_runner::REPLY_SENDER.scope("sales".to_string(), async {
                (
                    crate::claude_runner::reply_sender(),
                    crate::claude_runner::reply_sender_for("telegram", "555"),
                    crate::claude_runner::reply_sender_for("telegram", "999"),
                )
            }),
        )
        .await;
    assert_eq!(got.0.as_deref(), Some("sales"));
    assert_eq!(got.1.as_deref(), Some("sales"));
    assert_eq!(got.2, None, "another chat keeps the default bot");
    let bad = crate::claude_runner::REPLY_SENDER
        .scope("../etc".to_string(), async {
            crate::claude_runner::reply_sender()
        })
        .await;
    assert_eq!(bad, None, "only a valid employee id counts");
}

#[tokio::test]
async fn a_delegated_answer_returns_through_the_own_bot_and_into_its_bucket() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let mut fake = FakeTelegramServer::start().await;
    let own = fake.new_token(7400001);
    let other = fake.new_token(7400002);
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    write_agent(
        &agents,
        "sales",
        "業務",
        "@sales",
        "specialist",
        &telegram_section(home.path(), &own),
    );
    // The sub-agent has its own bot too; the answer must NOT use it.
    write_agent(
        &agents,
        "hr",
        "人資",
        "@hr",
        "specialist",
        &telegram_section(home.path(), &other),
    );
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    // The parent turn lives in sales' bucket of the DM.
    let sessions = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
    sessions
        .get_or_create("telegram:555#agent:sales", "sales")
        .await
        .unwrap();

    add_callback(
        home.path(),
        "m-own",
        "sales",
        "telegram",
        "555",
        Some("sales"),
    );
    crate::dispatcher::forward_delegation_response(home.path(), "m-own", "請假要提前三天", "hr")
        .await;
    let sent = reply_texts(&fake.state.sent_by(&own));
    assert!(
        sent.iter()
            .any(|(chat, t)| chat == "555" && t.contains("請假要提前三天")),
        "{sent:?}"
    );
    assert!(
        reply_texts(&fake.state.sent_by(&other)).is_empty(),
        "never through another employee's bot"
    );
    let history = sessions
        .get_messages("telegram:555#agent:sales")
        .await
        .unwrap();
    assert!(
        history.iter().any(|m| m.content.contains("請假要提前三天")),
        "the answer lands in the parent's own bucket"
    );

    // Sender's bot removed meanwhile: the forward fails (and is retried),
    // it does not fall back to another bot.
    write_agent(&agents, "sales", "業務", "@sales", "specialist", "");
    add_callback(
        home.path(),
        "m-gone",
        "sales",
        "telegram",
        "555",
        Some("sales"),
    );
    crate::dispatcher::forward_delegation_response(home.path(), "m-gone", "第二則", "hr").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let all: Vec<String> = [own.as_str(), other.as_str()]
        .iter()
        .flat_map(|t| reply_texts(&fake.state.sent_by(t)))
        .map(|(_, t)| t)
        .collect();
    assert!(!all.iter().any(|t| t.contains("第二則")), "{all:?}");
}

#[tokio::test]
async fn reminders_and_tool_messages_use_the_origin_bot() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let mut fake = FakeTelegramServer::start().await;
    let own = fake.new_token(7500001);
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    write_agent(
        &agents,
        "sales",
        "業務",
        "@sales",
        "specialist",
        &telegram_section(home.path(), &own),
    );
    let http = reqwest::Client::new();
    crate::reminder_scheduler::send_channel_message_as(
        home.path(),
        &http,
        "telegram",
        "555",
        "提醒：開會",
        Some("sales"),
    )
    .await
    .unwrap();
    assert!(
        reply_texts(&fake.state.sent_by(&own))
            .iter()
            .any(|(c, t)| c == "555" && t.contains("提醒：開會"))
    );
    // An employee without an own bot: error, no silent fallback.
    let err = crate::reminder_scheduler::send_channel_message_as(
        home.path(),
        &http,
        "telegram",
        "555",
        "x",
        Some("dudu"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("own Telegram bot"), "{err}");
    // A legacy reminder (no sender field) still deserialises.
    let legacy: crate::reminder_scheduler::Reminder = serde_json::from_value(json!({
        "id": "r1", "trigger_at": "2026-10-23T00:00:00Z", "channel": "telegram", "chat_id": "555"
    }))
    .unwrap();
    assert_eq!(legacy.sender_agent, None);
}

#[tokio::test]
async fn google_chat_sends_pick_the_app_that_is_in_the_space() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let fake = gchat::start_fake().await;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let email = format!("app-r-{id}@p0-test.iam.gserviceaccount.com");
    let shared_email = format!("shared-{id}@p0-test.iam.gserviceaccount.com");
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    let sa = gchat::service_account(&fake, &email);
    write_agent(
        &agents,
        "sales",
        "業務",
        "@sales",
        "specialist",
        &gchat::googlechat_section(home.path(), "666666666", &sa),
    );
    // A shared app is configured as well.
    let shared = gchat::service_account(&fake, &shared_email);
    let enc = crate::config_crypto::encrypt_value(&shared, home.path()).unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        format!("[channels]\ngooglechat_project_number = \"777777777\"\ngooglechat_service_account_json_enc = \"{enc}\"\n"),
    )
    .unwrap();
    let ctx = super::e2e_tests::reply_ctx(home.path()).await;
    super::e2e_seam::arm(home.path());
    let app = crate::googlechat_agents::router(ctx.clone());
    let event = gchat::message_event("spaces/OWNONLY", "hi");
    assert_eq!(
        gchat::post(&app, "sales", Some(&gchat::chat_jwt("666666666")), &event).await,
        axum::http::StatusCode::OK
    );
    let own_bearer = format!("Bearer tok-{email}");
    gchat::wait_text(&fake, &own_bearer, "OWN_BOT_REPLY").await;

    // A send with no explicit sender (approval card, goal progress, cron)
    // into that space goes through the app that is a member: sales' app.
    crate::googlechat::send_text_to_space(home.path(), "spaces/OWNONLY", "審批通知")
        .await
        .unwrap();
    gchat::wait_text(&fake, &own_bearer, "審批通知").await;
    // A space never seen through an own app keeps the shared app.
    crate::googlechat::send_text_to_space(home.path(), "spaces/OTHER", "共用通知")
        .await
        .unwrap();
    gchat::wait_text(&fake, &format!("Bearer tok-{shared_email}"), "共用通知").await;

    // M1: the employee's app is configured but unusable ⇒ error, not the
    // shared app.
    let broken = "[channels.googlechat]\nproject_number = \"666666666\"\nservice_account_json_enc = \"not-ciphertext\"\n";
    write_agent(&agents, "sales", "業務", "@sales", "specialist", broken);
    crate::googlechat_agents::forget("sales");
    let err = crate::googlechat::send_text_to_space_as(
        home.path(),
        Some("sales"),
        "spaces/OWNONLY",
        "不應送出",
    )
    .await
    .unwrap_err();
    assert!(err.contains("could not be loaded"), "{err}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !gchat::texts_with_bearer(&fake, &format!("Bearer tok-{shared_email}"))
            .iter()
            .any(|t| t.contains("不應送出")),
        "no silent fallback to the shared app"
    );
}
