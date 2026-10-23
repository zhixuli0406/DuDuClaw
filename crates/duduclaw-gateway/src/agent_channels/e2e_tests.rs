//! End-to-end: two AI employees, each with its own Telegram bot, against a
//! local fake Bot API (getMe / getUpdates / sendMessage / editMessageText /
//! deleteMessage). The real poller, message handler and reply pipeline run;
//! only the model turn is replaced by `e2e_seam`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::Uri;
use serde_json::{Value, json};

use crate::channel_reply::ReplyContext;
use crate::test_channel_provider::{LocalProviderRoute, register_local_route};

#[derive(Default)]
pub(crate) struct FakeTelegram {
    updates: Mutex<HashMap<String, VecDeque<Value>>>,
    sent: Mutex<Vec<(String, String, Value)>>,
    revoked: Mutex<HashSet<String>>,
}

impl FakeTelegram {
    pub(crate) fn push_update(&self, token: &str, update: Value) {
        self.updates
            .lock()
            .unwrap()
            .entry(token.into())
            .or_default()
            .push_back(update);
    }
    pub(crate) fn revoke(&self, token: &str) {
        self.revoked.lock().unwrap().insert(token.into());
    }
    /// `(method, body)` the bot of `token` sent.
    pub(crate) fn sent_by(&self, token: &str) -> Vec<(String, Value)> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _, _)| t == token)
            .map(|(_, m, b)| (m.clone(), b.clone()))
            .collect()
    }
}

fn bot_id(token: &str) -> i64 {
    token.split(':').next().unwrap().parse().unwrap()
}

async fn fake_tg(State(f): State<Arc<FakeTelegram>>, uri: Uri, body: Bytes) -> axum::Json<Value> {
    let path = uri.path().trim_start_matches("/bot");
    let (token, method) = path.rsplit_once('/').unwrap_or((path, ""));
    if f.revoked.lock().unwrap().contains(token) {
        return axum::Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"}));
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let result = match method {
        "getMe" => {
            json!({"id": bot_id(token), "is_bot": true, "first_name": "Bot", "username": format!("bot_{}", bot_id(token))})
        }
        "getUpdates" => {
            let next = f
                .updates
                .lock()
                .unwrap()
                .get_mut(token)
                .and_then(|q| q.pop_front());
            match next {
                Some(u) => json!([u]),
                None => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    json!([])
                }
            }
        }
        "sendMessage" => {
            f.sent
                .lock()
                .unwrap()
                .push((token.into(), method.into(), body.clone()));
            json!({"message_id": 900 + f.sent.lock().unwrap().len(), "chat": {"id": body["chat_id"]}, "date": 0})
        }
        "editMessageText" | "deleteMessage" => {
            f.sent
                .lock()
                .unwrap()
                .push((token.into(), method.into(), body.clone()));
            json!(true)
        }
        _ => json!(true),
    };
    axum::Json(json!({"ok": true, "result": result}))
}

/// A running fake Bot API; tokens are routed to it with [`Self::route`].
pub(crate) struct FakeTelegramServer {
    pub state: Arc<FakeTelegram>,
    pub origin: String,
    routes: Vec<LocalProviderRoute>,
}

impl FakeTelegramServer {
    pub(crate) async fn start() -> Self {
        let state = Arc::new(FakeTelegram::default());
        let app = Router::new().fallback(fake_tg).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            state,
            origin,
            routes: Vec::new(),
        }
    }
    /// A fresh Telegram-shaped token routed to this fake.
    pub(crate) fn new_token(&mut self, id: u32) -> String {
        let token = format!("{id}:p0-test-{}", uuid::Uuid::new_v4().simple());
        self.routes.push(register_local_route(&token, &self.origin));
        token
    }
}

pub(crate) fn dm(update_id: i64, user: i64, text: &str) -> Value {
    json!({"update_id": update_id, "message": {
        "message_id": update_id, "chat": {"id": user, "type": "private"},
        "from": {"id": user, "first_name": "Human"}, "text": text}})
}

pub(crate) fn telegram_section(home: &Path, token: &str) -> String {
    let enc = crate::config_crypto::encrypt_value(token, home).unwrap();
    format!("\n[channels.telegram]\nbot_token_enc = \"{enc}\"\n")
}

pub(crate) async fn reply_ctx(home: &Path) -> Arc<ReplyContext> {
    let mut reg = duduclaw_agent::registry::AgentRegistry::new(home.join("agents"));
    reg.scan().await.unwrap();
    let sessions =
        Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
    let (tx, _) = tokio::sync::broadcast::channel(64);
    let status = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
    Arc::new(ReplyContext::new(
        Arc::new(tokio::sync::RwLock::new(reg)),
        home.to_path_buf(),
        sessions,
        status,
        tx,
    ))
}

async fn wait_for<F: Fn() -> bool>(what: &str, f: F) {
    for _ in 0..150 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

fn reply_texts(sent: &[(String, Value)]) -> Vec<String> {
    sent.iter()
        .filter(|(m, _)| m == "sendMessage" || m == "editMessageText")
        .filter_map(|(_, b)| b["text"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn two_employees_with_their_own_bots_answer_independently() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let mut fake = FakeTelegramServer::start().await;
    let token_sales = fake.new_token(7100001);
    let token_hr = fake.new_token(7100002);
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    write_agent(
        &agents,
        "sales",
        "業務",
        "@sales",
        "specialist",
        &telegram_section(home.path(), &token_sales),
    );
    write_agent(
        &agents,
        "hr",
        "人資",
        "@hr",
        "specialist",
        &telegram_section(home.path(), &token_hr),
    );
    let ctx = reply_ctx(home.path()).await;
    super::e2e_seam::arm(home.path());

    // The same person DMs both bots: Telegram gives them the same chat id.
    fake.state
        .push_update(&token_sales, dm(1, 555, "業務請回答"));
    fake.state.push_update(&token_hr, dm(1, 555, "人資請回答"));
    let handles = crate::telegram::start_telegram_bots(home.path(), ctx.clone()).await;
    let labels: Vec<&str> = handles.iter().map(|(l, _)| l.as_str()).collect();
    assert!(
        labels.contains(&"telegram:sales") && labels.contains(&"telegram:hr"),
        "{labels:?}"
    );

    let st = fake.state.clone();
    let (ts, th) = (token_sales.clone(), token_hr.clone());
    wait_for("both bots to answer", || {
        let ok = |t: &str| {
            reply_texts(&st.sent_by(t))
                .iter()
                .any(|x| x.contains("OWN_BOT_REPLY"))
        };
        ok(&ts) && ok(&th)
    })
    .await;
    let sales = reply_texts(&fake.state.sent_by(&token_sales)).join("\n");
    let hr = reply_texts(&fake.state.sent_by(&token_hr)).join("\n");
    assert!(
        sales.contains("agent=sales session=telegram:555#agent:sales"),
        "{sales}"
    );
    assert!(
        hr.contains("agent=hr session=telegram:555#agent:hr"),
        "{hr}"
    );
    assert!(!sales.contains("SESSION_CLASH") && !hr.contains("SESSION_CLASH"));
    assert!(
        !sales.contains("agent=hr") && !hr.contains("agent=sales"),
        "replies never cross bots"
    );
    assert_eq!(
        super::identity::get("telegram:sales").as_deref(),
        Some("bot_7100001")
    );

    // The private DM bucket is still a private conversation.
    assert!(duduclaw_core::is_private_session(
        "telegram:555#agent:sales",
        "555"
    ));

    // Token revoked mid-run: the poller stops and says so.
    fake.state.revoke(&token_hr);
    let status = ctx.channel_status.clone();
    for _ in 0..200 {
        let done = status.read().await.get("telegram:hr").is_some_and(|s| {
            !s.connected
                && s.error
                    .as_deref()
                    .is_some_and(|e| e.contains("Bot Token 無效"))
        });
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let hr_state = status
        .read()
        .await
        .get("telegram:hr")
        .cloned()
        .expect("status");
    assert!(!hr_state.connected, "revoked bot is offline");
    assert!(
        hr_state
            .error
            .unwrap_or_default()
            .contains("Bot Token 無效")
    );
    for (_, h) in handles {
        h.abort();
    }
}

#[tokio::test]
async fn a_bot_left_on_two_employees_starts_once_and_marks_the_other() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    let mut fake = FakeTelegramServer::start().await;
    let token = fake.new_token(7200001);
    use crate::trigger_route::tests::write_agent;
    write_agent(&agents, "dudu", "總機", "@dudu", "main", "");
    write_agent(
        &agents,
        "a-first",
        "甲",
        "@a",
        "specialist",
        &telegram_section(home.path(), &token),
    );
    write_agent(
        &agents,
        "b-second",
        "乙",
        "@b",
        "specialist",
        &telegram_section(home.path(), &token),
    );
    let ctx = reply_ctx(home.path()).await;
    let handles = crate::telegram::start_telegram_bots(home.path(), ctx.clone()).await;
    assert_eq!(handles.len(), 1, "one poller per bot");
    let map = ctx.channel_status.read().await;
    let (started, skipped) = if handles[0].0 == "telegram:a-first" {
        ("telegram:a-first", "telegram:b-second")
    } else {
        ("telegram:b-second", "telegram:a-first")
    };
    assert_eq!(handles[0].0, started);
    let s = map.get(skipped).expect("the skipped employee is marked");
    assert!(
        !s.connected
            && s.error
                .as_deref()
                .unwrap_or("")
                .contains("一個 Bot 只能給一位員工")
    );
    drop(map);
    for (_, h) in handles {
        h.abort();
    }
}
