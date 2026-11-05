//! Pull transport for Chat. Only the gateway lock holder consumes messages.
//! ACK follows durable inbox commit. Processing side effects are not exactly-once.
use std::{collections::{HashMap, VecDeque}, path::Path, sync::Arc, time::Duration};
use base64::Engine;
use serde_json::{Value, json};
use crate::{channel_reply::ReplyContext, googlechat::{GoogleChatCreds, GoogleChatState}};
use crate::agent_channels::{googlechat_config, store};
use crate::googlechat_inbox::{Inbox, Admission, MAX_MESSAGE_AGE_SECS, CLOCK_SKEW_SECS};

#[derive(Clone, PartialEq, Eq)]
struct Subscription {
    name: String,
    key: String,
    project_number: String,
}

async fn shared_table(home: &Path) -> Option<toml::Table> {
    tokio::fs::read_to_string(home.join("config.toml")).await.ok()?.parse().ok()
}

/// An absent setting on a readable config preserves the existing HTTP mode.
pub(crate) async fn shared_http_enabled(home: &Path) -> bool {
    if !crate::googlechat::shared_settings_valid(home).await { return false; }
    let Some(table) = shared_table(home).await else { return false; };
    let Some(channels) = table.get("channels").and_then(toml::Value::as_table) else { return false; };
    let connection = match channels.get("googlechat_connection") {
        None => "http",
        Some(value) => match value.as_str() { Some(value) => value, None => return false },
    };
    let subscription = match channels.get("googlechat_pubsub_subscription") {
        None => "",
        Some(value) => match value.as_str() { Some(value) => value, None => return false },
    };
    connection == "http" && subscription.is_empty()
}

async fn configured(ctx: &ReplyContext, agent: Option<&str>) -> Option<Subscription> {
    if let Some(id) = agent {
        if ctx.registry.read().await.get(id).is_none() { return None; }
    }
    subscription(&ctx.home_dir, agent).await
}

async fn subscription(home: &Path, agent: Option<&str>) -> Option<Subscription> {
    crate::product_policy::require_channel(home, "googlechat").ok()?;
    let (connection, name, key, project_number) = match agent {
        Some(id) => {
            if !store::agent_uses_pubsub(home, id) { return None; }
            let app = store::agent_chat_app(home, id).await?;
            (app.connection, app.pubsub_subscription, app.service_account.expose_owned(), app.project_number)
        }
        None => {
            if !crate::googlechat::shared_settings_valid(home).await { return None; }
            let table = shared_table(home).await?;
            let ch = table.get("channels")?;
            if ch.get("googlechat_connection")?.as_str()? != "pubsub" { return None; }
            let key = crate::config_crypto::decrypt_config_field_async(
                &table, "channels", "googlechat_service_account_json", home).await?;
            (ch.get("googlechat_connection")?.as_str()?.to_string(),
             ch.get("googlechat_pubsub_subscription")?.as_str()?.to_string(), key.expose_owned(),
             ch.get("googlechat_project_number")?.as_str()?.to_string())
        }
    };
    if connection != "pubsub" { return None; }
    googlechat_config::validate_project_number(&project_number).ok()?;
    // Validate inside the worker as well, so invalid bindings have a visible status.
    Some(Subscription { name, key, project_number })
}

pub(crate) async fn shared_pubsub_configured(home: &Path) -> bool {
    subscription(home, None).await.is_some()
}

/// Reconcile workers on removal, credential rotation, transport changes and
/// lock loss. Aborting a pull leaves unacknowledged messages for redelivery.
pub fn spawn(ctx: Arc<ReplyContext>) {
    tokio::spawn(async move {
        let mut workers: HashMap<Option<String>, (Subscription, tokio::task::JoinHandle<()>)> = HashMap::new();
        loop {
            let mut desired = HashMap::new();
            let mut invalid = Vec::new();
            if duduclaw_core::gateway_instance::held(&ctx.home_dir) {
                let mut agents = vec![None];
                agents.extend(ctx.registry.read().await.list().into_iter()
                    .map(|a| Some(a.config.agent.name.clone())));
                for agent in agents {
                    if let Some(config) = configured(&ctx, agent.as_deref()).await {
                        desired.insert(agent, config);
                    } else {
                        let wants_pubsub = match agent.as_deref() {
                            Some(id) => store::agent_uses_pubsub(&ctx.home_dir, id),
                            None => shared_table(&ctx.home_dir).await.and_then(|t| t.get("channels")?.get("googlechat_connection")?.as_str().map(str::to_owned)).as_deref() == Some("pubsub"),
                        };
                        if wants_pubsub { invalid.push(agent); }
                    }
                }
            }
            let stale: Vec<_> = workers.iter().filter(|(id, (config, task))|
                desired.get(*id) != Some(config) || task.is_finished()).map(|(id, _)| id.clone()).collect();
            for id in stale {
                if let Some((_, task)) = workers.remove(&id) {
                    task.abort(); let _ = task.await;
                    let _bind = crate::agent_channels::binding_lock().await;
                    let http_active = match id.as_deref() {
                        Some(agent) => ctx.registry.read().await.get(agent).is_some()
                            && store::agent_chat_app(&ctx.home_dir, agent).await.is_some_and(|app| app.connection == "http"),
                        None => shared_http_enabled(&ctx.home_dir).await
                            && crate::config_crypto::read_encrypted_config_field(&ctx.home_dir, "channels", "googlechat_project_number").await.is_some_and(|p| !p.trim().is_empty())
                            && GoogleChatCreds::from_config(&ctx.home_dir).await.is_some(),
                    };
                    if !http_active {
                        let label = id.as_deref().map(|id| crate::agent_channels::label("googlechat", id)).unwrap_or_else(|| "googlechat".into());
                        ctx.channel_status.write().await.remove(&label);
                    }
                }
            }
            for id in invalid {
                let label = id.as_deref().map(|id| crate::agent_channels::label("googlechat", id)).unwrap_or_else(|| "googlechat".into());
                crate::channel_reply::set_channel_connected(&ctx.channel_status, &label, false,
                    Some("Google Chat Pub/Sub 設定無效：請確認服務帳戶、金鑰 project_id、email 專案與訂閱專案一致。".into()), Some(&ctx.event_tx)).await;
            }
            for (id, config) in desired {
                workers.entry(id.clone()).or_insert_with(|| {
                    let task = tokio::spawn(run(ctx.clone(), id, config.clone()));
                    (config, task)
                });
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn still_current(ctx: &ReplyContext, agent: Option<&str>, expected: &Subscription) -> bool {
    duduclaw_core::gateway_instance::held(&ctx.home_dir)
        && configured(ctx, agent).await.as_ref() == Some(expected)
}

async fn run(ctx: Arc<ReplyContext>, agent: Option<String>, config: Subscription) {
    let creds = match agent.as_deref() {
        Some(id) => GoogleChatCreds::for_agent(&config.key, &ctx.home_dir, id),
        None => GoogleChatCreds::from_service_account_json(&config.key, &ctx.home_dir),
    };
    let Some(creds) = creds else { return; };
    let state = Arc::new(GoogleChatState { ctx: ctx.clone(), creds: Arc::new(creds), agent: agent.clone() });
    let label = agent.as_deref().map(|id| crate::agent_channels::label("googlechat", id))
        .unwrap_or_else(|| "googlechat".into());
    let mut delay = 1u64;
    let mut batch = VecDeque::new();
    let mut verified = false;
    let mut last_prune = std::time::Instant::now();
    let inbox = match Inbox::shared(&ctx.home_dir) {
        Ok(inbox) => inbox,
        Err(e) => {
            crate::channel_reply::set_channel_connected(&ctx.channel_status, &label, false, Some(e), Some(&ctx.event_tx)).await;
            return;
        }
    };
    while still_current(&ctx, agent.as_deref(), &config).await {
        if last_prune.elapsed() >= Duration::from_secs(60) {
            if let Err(e) = inbox.prune(chrono::Utc::now().timestamp()) {
                tracing::error!(error = e, "Google Chat inbox pruning failed; stopping worker");
                return;
            }
            last_prune = std::time::Instant::now();
        }
        let result = if !verified {
            match googlechat_config::verify_subscription(&state.creds, &config.key, &config.name).await {
                Ok(()) => { verified = true; Ok(()) },
                Err(e) => Err(e),
            }
        } else {
            let base = format!("https://pubsub.googleapis.com/v1/{}", config.name);
            pull_once_at(&state, &config, &base, &inbox, &mut batch).await
        };
        let diagnostic = inbox.diagnostic(&stream(&state, &config)).unwrap_or_else(Some);
        {
            let _bind = crate::agent_channels::binding_lock().await;
            if !still_current(&ctx, agent.as_deref(), &config).await { break; }
            crate::channel_reply::set_channel_connected(&ctx.channel_status, &label, result.is_ok() && diagnostic.is_none(),
                result.as_ref().err().cloned().or(diagnostic), Some(&ctx.event_tx)).await;
        }
        match result {
            Ok(()) => { delay = 1; tokio::time::sleep(Duration::from_millis(250)).await; }
            Err(e) => {
                tracing::warn!(channel = label, error = e, "Google Chat Pub/Sub pull failed");
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = next_backoff(delay);
            }
        }
    }
}

fn next_backoff(delay: u64) -> u64 { delay.saturating_mul(2).min(30) }

fn stream(state: &GoogleChatState, config: &Subscription) -> String {
    // Subscription and service-account rotation must not reset an app's replay evidence.
    json!([state.agent, config.project_number]).to_string()
}

/// Replay references after re-reading Chat; notification content never enters a turn.
async fn drain(state: &Arc<GoogleChatState>, config: &Subscription, inbox: &Arc<Inbox>) -> Result<(), String> {
    drain_verified(state, config, inbox, None).await
}

async fn drain_verified(state: &Arc<GoogleChatState>, config: &Subscription, inbox: &Arc<Inbox>,
    verified: Option<(&str, &Value)>) -> Result<(), String> {
    let stream = stream(state, config);
    for row in inbox.pending(&stream)? {
        if !still_current(&state.ctx, state.agent.as_deref(), config).await { break; }
        let reference: Value = serde_json::from_str(&row.payload).map_err(|_| "Google Chat 收件匣事件格式錯誤")?;
        let fetched = match verified {
            Some((id, event)) if id == row.id => Ok(event.clone()),
            _ => authoritative_event(&state.creds, &reference).await,
        };
        let event = match fetched {
            Ok(event) => event,
            Err(reason) => {
                tracing::warn!(agent = ?state.agent, reason, "Google Chat Pub/Sub notification dropped; no turn");
                inbox.discard_pending(&stream, &row.id)?;
                continue;
            }
        };
        let space = event.pointer("/message/space/name").and_then(Value::as_str).unwrap_or("");
        let sender = event.pointer("/message/sender/name").and_then(Value::as_str).unwrap_or("");
        let group = json!([state.agent, space]).to_string();
        let conversation = json!([state.agent, space, sender]).to_string();
        let bind = crate::agent_channels::binding_lock().await;
        if !still_current(&state.ctx, state.agent.as_deref(), config).await { break; }
        if !inbox.transition(&stream, &row.id, "pending", "queued")? { continue; }
        let (st, cfg, db, key, id) = (state.clone(), config.clone(), inbox.clone(), stream.clone(), row.id.clone());
        if state.ctx.googlechat_queue.submit_in(&group, &conversation, async move {
            let bind = crate::agent_channels::binding_lock().await;
            if !still_current(&st.ctx, st.agent.as_deref(), &cfg).await {
                let _ = db.transition(&key, &id, "queued", "pending");
                return;
            }
            if !db.transition(&key, &id, "queued", "running").unwrap_or(false) { return; }
            drop(bind);
            crate::googlechat::process_pubsub_event(&st, &event).await;
            if let Err(e) = db.transition(&key, &id, "running", "done") {
                tracing::error!(error = e, "Google Chat inbox completion failed");
            }
        }).is_err() {
            // Match HTTP overload behavior: ACK/drop, never retry a full queue.
            inbox.transition(&stream, &row.id, "queued", "dropped")?;
            tracing::warn!(agent = ?state.agent, "Google Chat Pub/Sub queue full: ACK and drop; no turn");
        }
        drop(bind);
    }
    Ok(())
}

/// A bounded batch stays outstanding until ACK succeeds or its IDs are invalid.
/// Database failures retain it; invalid or overloaded notifications are ACKed.
async fn pull_once_at(state: &Arc<GoogleChatState>, config: &Subscription, base: &str,
    inbox: &Arc<Inbox>, batch: &mut VecDeque<Value>) -> Result<(), String> {
    if !still_current(&state.ctx, state.agent.as_deref(), config).await { return Ok(()); }
    let stream = stream(state, config);
    drain(state, config, inbox).await?;
    if batch.is_empty() {
        let token = state.creds.pubsub_token().await?;
        let mut response = state.creds.http.post(format!("{base}:pull")).bearer_auth(&token)
            .json(&json!({"maxMessages": 10, "returnImmediately": false})).send().await
            .map_err(|_| "Pub/Sub 拉取連線失敗".to_string())?;
        if !response.status().is_success() { return Err(format!("Pub/Sub 拉取失敗：{}", response.status())); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "Pub/Sub 回應讀取失敗")? {
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 { return Err("Pub/Sub 回應過大".into()); }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| "Pub/Sub 回應格式錯誤")?;
        match body.get("receivedMessages") {
            None if body.is_object() => return Ok(()),
            Some(Value::Array(items)) if items.len() <= 10 => batch.extend(items.iter().cloned()),
            _ => return Err("Pub/Sub 事件批次格式錯誤".into()),
        }
    }
    let mut acknowledgements = Vec::new();
    for message in batch.iter() {
        if !still_current(&state.ctx, state.agent.as_deref(), config).await { return Ok(()); }
        let Some(ack_id) = message.get("ackId").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 4096) else {
            continue;
        };
        if let Some(event) = decode_payload(message) {
            if crate::googlechat::is_addon_event(&event) {
                // Pub/Sub payloads are unverified: they cannot diagnose or
                // change channel state. Do not log any payload content.
                tracing::warn!(agent = ?state.agent, "Google Chat Pub/Sub unverified add-on notification: ACK and drop; no turn");
            } else if let Some(reference) = notification_reference(&event) {
                let key = event_key(&reference);
                if !inbox.contains(&stream, &key)? {
                    match authoritative_event(&state.creds, &reference).await {
                        Ok(verified) => {
                            let bind = crate::agent_channels::binding_lock().await;
                            if !still_current(&state.ctx, state.agent.as_deref(), config).await { return Ok(()); }
                            let admission = inbox.persist(&stream, &key, &reference.to_string())?;
                            drop(bind);
                            match admission {
                                Admission::Inserted => drain_verified(state, config, inbox, Some((&key, &verified))).await?,
                                Admission::Duplicate => {},
                                Admission::Full => tracing::warn!(agent = ?state.agent, "Google Chat Pub/Sub inbox full: ACK and drop; no turn"),
                            }
                        }
                        Err(reason) => tracing::warn!(agent = ?state.agent, reason, "Google Chat Pub/Sub notification dropped; no turn"),
                    }
                }
            } else {
                tracing::warn!(agent = ?state.agent, "Google Chat Pub/Sub unsupported or invalid notification dropped; no turn");
            }
        } else {
            tracing::warn!(agent = ?state.agent, "Google Chat Pub/Sub malformed notification dropped; no turn");
        }
        acknowledgements.push(ack_id.to_string());
    }
    if !still_current(&state.ctx, state.agent.as_deref(), config).await { return Ok(()); }
    if !acknowledgements.is_empty() {
        let token = state.creds.pubsub_token().await?;
        let response = state.creds.http.post(format!("{base}:acknowledge")).bearer_auth(&token)
            .json(&json!({"ackIds": acknowledgements})).send().await
            .map_err(|_| "Pub/Sub 確認接收連線失敗")?;
        if response.status() == reqwest::StatusCode::BAD_REQUEST {
            // Expired ACK IDs cannot recover. Keep durable replay evidence and
            // pull a fresh delivery instead of retrying these IDs forever.
            batch.clear();
            return Err("Pub/Sub 確認接收識別碼無效或已過期，將重新拉取通知。".into());
        }
        if !response.status().is_success() { return Err(format!("Pub/Sub 確認接收失敗：{}", response.status())); }
    }
    batch.clear();
    Ok(())
}

fn decode_payload(message: &Value) -> Option<Value> {
    let encoded = message.pointer("/message/data")?.as_str()?;
    if encoded.len() > 1400000 { return None; }
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    if bytes.len() > 1024 * 1024 { return None; }
    serde_json::from_slice(&bytes).ok()
}

fn resource(value: &str, prefix: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 2 && parts[0] == prefix && identifier(parts[1], false)
}

fn identifier(value: &str, dots: bool) -> bool {
    (1..=256).contains(&value.len()) && value != "." && value != ".."
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c) || (dots && c == b'.'))
}

fn message_space(name: &str) -> Option<String> {
    let parts: Vec<_> = name.split('/').collect();
    if parts.len() != 4 || parts[0] != "spaces" || parts[2] != "messages"
        || !identifier(parts[1], false) || !identifier(parts[3], true) { return None; }
    Some(format!("spaces/{}", parts[1]))
}

/// Only the fetch locator survives the untrusted payload. ADDED_TO_SPACE
/// and every other non-fetchable event are ignored without effects.
fn notification_reference(event: &Value) -> Option<Value> {
    if event.get("type")?.as_str()? != "MESSAGE" { return None; }
    let name = event.pointer("/message/name")?.as_str()?;
    let space = message_space(name)?;
    for pointer in ["/space/name", "/message/space/name"] {
        if let Some(value) = event.pointer(pointer) {
            if value.as_str() != Some(space.as_str()) { return None; }
        }
    }
    Some(json!({"type":"MESSAGE", "message":{"name":name}, "space":{"name":space}}))
}

fn event_key(reference: &Value) -> String {
    format!("MESSAGE:{}", reference.pointer("/message/name").and_then(Value::as_str).unwrap_or(""))
}

fn creation_time_is_current(created: i64, now: i64) -> bool {
    created >= now - MAX_MESSAGE_AGE_SECS && created <= now + CLOCK_SKEW_SECS
}

async fn authoritative_event(creds: &GoogleChatCreds, reference: &Value) -> Result<Value, String> {
    let reference = notification_reference(reference).ok_or("Chat resource 名稱不合法")?;
    let name = reference.pointer("/message/name").and_then(Value::as_str).ok_or("缺少 Chat resource 名稱")?;
    let space = message_space(name).ok_or("Chat resource 名稱不合法")?;
    let token = creds.token().await.map_err(|_| "Google Chat 訊息回讀授權失敗；需要 chat.bot scope")?;
    let mut response = creds.http.get(format!("{}/{name}", creds.api_base())).bearer_auth(token)
        .send().await.map_err(|_| "Google Chat 訊息回讀連線失敗")?;
    if !response.status().is_success() {
        return Err(format!("Google Chat 訊息回讀失敗（{}）；請確認 Chat app 可讀此訊息且使用 chat.bot scope；403/404 不會建立對話", response.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "Google Chat 訊息回讀失敗")? {
        if bytes.len() + chunk.len() > 1024 * 1024 { return Err("Google Chat 訊息回讀資料過大".into()); }
        bytes.extend_from_slice(&chunk);
    }
    let message: Value = serde_json::from_slice(&bytes).map_err(|_| "Google Chat 訊息回讀格式錯誤")?;
    if message.get("name").and_then(Value::as_str) != Some(name)
        || message.pointer("/space/name").and_then(Value::as_str) != Some(space.as_str()) {
        return Err("Google Chat 回讀的訊息或聊天室與通知綁定不符".into());
    }
    if !message.pointer("/sender/name").and_then(Value::as_str).is_some_and(|s| resource(s, "users"))
        || message.pointer("/sender/type").and_then(Value::as_str) != Some("HUMAN") {
        return Err("Google Chat 回讀未提供可驗證的人類寄件者".into());
    }
    if let Some(thread) = message.pointer("/thread/name") {
        let valid = thread.as_str().is_some_and(|t| {
            let parts: Vec<_> = t.split('/').collect();
            parts.len() == 4 && format!("{}/{}", parts[0], parts[1]) == space
                && parts[2] == "threads" && identifier(parts[3], true)
        });
        if !valid { return Err("Google Chat 回讀的討論串不屬於此聊天室".into()); }
    }
    // The trusted createTime closes replay after dedupe retention expires.
    let created = message.get("createTime").and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|t| t.timestamp())
        .ok_or("Google Chat 回讀缺少有效建立時間")?;
    let now = chrono::Utc::now().timestamp();
    if !creation_time_is_current(created, now) {
        return Err("Google Chat 訊息超出七天接收期限或建立時間無效".into());
    }
    Ok(json!({"type":"MESSAGE", "space":message["space"], "message":message}))
}

#[cfg(all(test, unix))]
mod googlechat_pubsub_tests {
    use super::*;
    use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
    use axum::{Router, body::Bytes, extract::State, http::{Method, StatusCode, Uri}, response::{IntoResponse, Response}};
    use crate::agent_channels::{e2e_gchat_tests as chat, e2e_tests, e2e_seam};

    struct Fake {
        delivery: Mutex<Vec<Value>>,
        message: Mutex<Value>,
        status: Mutex<StatusCode>,
        calls: Mutex<Vec<(Method, String, Value)>>,
        fail_ack: AtomicBool,
        invalid_ack: AtomicBool,
        exactly_once: AtomicBool,
    }
    async fn serve_fake(State(fake): State<Arc<Fake>>, method: Method, uri: Uri, body: Bytes) -> Response {
        fake.calls.lock().unwrap().push((method.clone(), uri.path().to_string(), serde_json::from_slice(&body).unwrap_or(Value::Null)));
        if method == Method::GET && uri.path() == "/v1/projects/test-project/subscriptions/chat-events" {
            axum::Json(json!({"name":"projects/test-project/subscriptions/chat-events",
                "topic":"projects/test-project/topics/duduclaw-chat-events",
                "enableExactlyOnceDelivery":fake.exactly_once.load(Ordering::SeqCst)})).into_response()
        } else if method == Method::GET {
            let status = *fake.status.lock().unwrap();
            (status, axum::Json(fake.message.lock().unwrap().clone())).into_response()
        } else if uri.path().ends_with(":pull") {
            axum::Json(json!({"receivedMessages": fake.delivery.lock().unwrap().clone()})).into_response()
        } else if uri.path().ends_with(":acknowledge") && fake.invalid_ack.swap(false, Ordering::SeqCst) {
            StatusCode::BAD_REQUEST.into_response()
        } else if uri.path().ends_with(":acknowledge") && fake.fail_ack.swap(false, Ordering::SeqCst) {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        } else if method == Method::POST {
            axum::Json(json!({"name":"spaces/AAA/messages/reply"})).into_response()
        } else { axum::Json(json!({})).into_response() }
    }

    fn api_message() -> Value {
        json!({"name":"spaces/AAA/messages/BBB", "space":{"name":"spaces/AAA", "type":"SPACE"},
            "sender":{"name":"users/42", "type":"HUMAN", "displayName":"Real sender"},
            "text":"Authoritative API content", "thread":{"name":"spaces/AAA/threads/THREAD"},
            "createTime":chrono::Utc::now().to_rfc3339()})
    }
    fn notification() -> Value {
        json!({"type":"MESSAGE", "space":{"name":"spaces/AAA", "type":"DM"},
            "message":{"name":"spaces/AAA/messages/BBB", "sender":{"name":"users/forged"},
                "text":"FORGED payload", "argumentText":"FORGED command", "attachment":[{"contentName":"forged"}]}})
    }
    fn delivery(id: &str, event: &Value) -> Value {
        json!({"ackId":format!("ack-{id}"), "message":{"messageId":id,
            "data":base64::engine::general_purpose::STANDARD.encode(event.to_string())}})
    }
    struct Harness {
        home: tempfile::TempDir,
        state: Arc<GoogleChatState>,
        config: Subscription,
        inbox: Arc<Inbox>,
        fake: Arc<Fake>,
        base: String,
        server: tokio::task::JoinHandle<()>,
    }
    impl Drop for Harness { fn drop(&mut self) { self.server.abort(); } }
    async fn harness() -> Harness {
        let home = tempfile::tempdir().unwrap();
        crate::trigger_route::tests::write_agent(&home.path().join("agents"), "dudu", "總機", "@dudu", "specialist", "");
        let google = chat::start_fake().await;
        let email = format!("pubsub-{}@test-project.iam.gserviceaccount.com", uuid::Uuid::new_v4().simple());
        let mut key: Value = serde_json::from_str(&chat::service_account(&google, &email)).unwrap();
        key["project_id"] = json!("test-project");
        let enc = crate::config_crypto::encrypt_value(&key.to_string(), home.path()).unwrap();
        tokio::fs::write(home.path().join("config.toml"), format!("[general]\ndefault_agent = \"dudu\"\n[channels]\ngooglechat_project_number = \"123456789\"\ngooglechat_connection = \"pubsub\"\ngooglechat_pubsub_subscription = \"projects/test-project/subscriptions/chat-events\"\ngooglechat_service_account_json_enc = \"{enc}\"\n")).await.unwrap();
        let mut ctx = e2e_tests::reply_ctx(home.path()).await;
        Arc::get_mut(&mut ctx).unwrap().googlechat_queue = crate::conversation_queue::ConversationQueue::with_caps(1, 1);
        let config = configured(&ctx, None).await.unwrap();
        let state = Arc::new(GoogleChatState { ctx, creds: Arc::new(GoogleChatCreds::from_config(home.path()).await.unwrap()), agent: None });
        let fake = Arc::new(Fake { delivery: Mutex::new(vec![delivery("one", &notification())]), message: Mutex::new(api_message()),
            status: Mutex::new(StatusCode::OK), calls: Mutex::new(Vec::new()), fail_ack: AtomicBool::new(false), invalid_ack: AtomicBool::new(false), exactly_once: AtomicBool::new(false) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let jwks = crate::googlechat::test_routes::jwks(&email).unwrap();
        crate::googlechat::test_routes::set(&email, &origin, &jwks);
        let router = Router::new().fallback(serve_fake).with_state(fake.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let inbox = Inbox::shared(home.path()).unwrap();
        Harness { home, state, config, inbox, fake, base: format!("{origin}/projects/test-project/subscriptions/chat-events"), server }
    }
    async fn pull(h: &Harness, batch: &mut VecDeque<Value>) -> Result<(), String> {
        pull_once_at(&h.state, &h.config, &h.base, &h.inbox, batch).await
    }
    fn acked(h: &Harness) -> Vec<Value> {
        h.fake.calls.lock().unwrap().iter().filter(|(_, path, _)| path.ends_with(":acknowledge")).map(|(_, _, body)| body.clone()).collect()
    }

    #[test]
    fn trusted_creation_time_keeps_seven_day_limit_and_skew_boundaries() {
        let now = 1_800_000_000;
        assert!(creation_time_is_current(now + CLOCK_SKEW_SECS, now));
        assert!(!creation_time_is_current(now + CLOCK_SKEW_SECS + 1, now));
        assert!(creation_time_is_current(now - MAX_MESSAGE_AGE_SECS, now));
        assert!(!creation_time_is_current(now - MAX_MESSAGE_AGE_SECS - 1, now));
        let created = now + CLOCK_SKEW_SECS;
        assert!(creation_time_is_current(created, created + MAX_MESSAGE_AGE_SECS));
        assert!(!creation_time_is_current(created, created + MAX_MESSAGE_AGE_SECS + 1));
    }

    #[tokio::test]
    async fn unverified_addon_payloads_ack_without_channel_state_or_events() {
        let h = harness().await;
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        let mut waiting = e2e_seam::arm_waiting(h.home.path());
        crate::channel_reply::set_channel_connected(&h.state.ctx.channel_status, "googlechat", true,
            None, None).await;
        let before = h.state.ctx.channel_status.read().await.get("googlechat").unwrap().clone();
        let mut events = h.state.ctx.event_tx.subscribe();
        let mut batch = VecDeque::new();
        for (n, addon) in [json!({"commonEventObject":{}}),
            json!({"chat":{"messagePayload":{"message":{"text":"untrusted"}}}})].into_iter().enumerate() {
            // Even a payload with a valid-looking MESSAGE locator must not
            // bypass the add-on drop path or trigger authoritative retrieval.
            let mut event = notification();
            event.as_object_mut().unwrap().extend(addon.as_object().unwrap().clone());
            *h.fake.delivery.lock().unwrap() = vec![delivery(&format!("addon{n}"), &event)];
            pull(&h, &mut batch).await.unwrap();
            assert!(batch.is_empty());
            let statuses = h.state.ctx.channel_status.read().await;
            let status = statuses.get("googlechat").unwrap();
            assert_eq!(status.connected, before.connected);
            assert_eq!(status.last_event, before.last_event);
            assert_eq!(status.error, before.error);
            drop(statuses);
            assert!(matches!(events.try_recv(), Err(tokio::sync::broadcast::error::TryRecvError::Empty)));
        }
        assert_eq!(acked(&h), vec![json!({"ackIds":["ack-addon0"]}), json!({"ackIds":["ack-addon1"]})]);
        assert!(waiting.entered.try_recv().is_err());
        assert!(h.inbox.pending(&stream(&h.state, &h.config)).unwrap().is_empty());
        assert!(!h.inbox.contains(&stream(&h.state, &h.config), &event_key(&notification())).unwrap());
        assert!(h.fake.calls.lock().unwrap().iter().all(|(method, _, _)| *method == Method::POST));
    }

    #[test]
    fn notification_locator_rejects_cross_space_traversal_and_non_message_events() {
        assert_eq!(notification_reference(&notification()).unwrap(), json!({"type":"MESSAGE","message":{"name":"spaces/AAA/messages/BBB"},"space":{"name":"spaces/AAA"}}));
        for name in ["spaces/AAA/messages/..", "spaces/AAA/messages/a%2fb", "spaces/AAA/messages/a?x", "spaces/AAA/messages/a/b", "spaces/AAA/messages/a#x", "spaces/../messages/a", "https://evil.test/a"] {
            let mut event = notification(); event["message"]["name"] = json!(name);
            assert!(notification_reference(&event).is_none(), "{name}");
        }
        let mut event = notification(); event["space"]["name"] = json!("spaces/OTHER");
        assert!(notification_reference(&event).is_none());
        event = notification(); event["type"] = json!("ADDED_TO_SPACE");
        assert!(notification_reference(&event).is_none());
        assert!(decode_payload(&json!({"message":{"data":"%bad"}})).is_none());
    }

    #[tokio::test]
    async fn forged_payload_uses_only_api_sender_text_attachments_and_thread() {
        let h = harness().await;
        let mut message = api_message();
        message["attachment"] = json!([{"contentName":"real attachment", "source":"DRIVE_FILE"}]);
        *h.fake.message.lock().unwrap() = message.clone();
        let event = authoritative_event(&h.state.creds, &notification()).await.unwrap();
        assert_eq!(event["message"], message);
        assert_eq!(event["space"]["type"], "SPACE");
        assert!(event["message"].get("argumentText").is_none());
        let calls = h.fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, Method::GET);
        assert_eq!(calls[0].1, "/spaces/AAA/messages/BBB");
    }

    #[tokio::test]
    async fn api_failure_cross_space_and_stale_messages_ack_without_turn() {
        let h = harness().await;
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        let mut waiting = e2e_seam::arm_waiting(h.home.path());
        let mut batch = VecDeque::new();
        for (n, status) in [StatusCode::FORBIDDEN, StatusCode::NOT_FOUND, StatusCode::INTERNAL_SERVER_ERROR].into_iter().enumerate() {
            let mut event = notification(); event["message"]["name"] = json!(format!("spaces/AAA/messages/error{n}"));
            *h.fake.delivery.lock().unwrap() = vec![delivery(&format!("error{n}"), &event)];
            *h.fake.status.lock().unwrap() = status;
            pull(&h, &mut batch).await.unwrap();
        }
        *h.fake.status.lock().unwrap() = StatusCode::OK;
        for (n, invalid) in ["space", "name", "thread", "sender", "time"].into_iter().enumerate() {
            let name = format!("spaces/AAA/messages/invalid{n}");
            let mut event = notification(); event["message"]["name"] = json!(name);
            let mut message = api_message(); message["name"] = json!(name);
            match invalid {
                "space" => message["space"]["name"] = json!("spaces/OTHER"),
                "name" => message["name"] = json!("spaces/OTHER/messages/BBB"),
                "thread" => message["thread"]["name"] = json!("spaces/OTHER/threads/T"),
                "sender" => message["sender"]["type"] = json!("BOT"),
                _ => message["createTime"] = json!((chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339()),
            }
            *h.fake.message.lock().unwrap() = message;
            *h.fake.delivery.lock().unwrap() = vec![delivery(&format!("invalid{n}"), &event)];
            pull(&h, &mut batch).await.unwrap();
        }
        assert_eq!(acked(&h).len(), 8);
        assert!(!h.inbox.contains(&stream(&h.state, &h.config), "MESSAGE:spaces/AAA/messages/error0").unwrap(), "unverified notifications cannot fill dedupe capacity");
        assert!(waiting.entered.try_recv().is_err());
        assert!(h.inbox.pending(&stream(&h.state, &h.config)).unwrap().is_empty());
        assert!(h.fake.calls.lock().unwrap().iter().all(|(_, path, _)| path != "/spaces/AAA/messages"), "no placeholder or reply");
    }

    #[tokio::test]
    async fn worker_start_refuses_exactly_once_before_pull() {
        let h = harness().await;
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        h.fake.exactly_once.store(true, Ordering::SeqCst);
        let worker = tokio::spawn(run(h.state.ctx.clone(), None, h.config.clone()));
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if h.state.ctx.channel_status.read().await.get("googlechat")
                    .is_some_and(|s| !s.connected && s.error.as_deref().is_some_and(|e| e.contains("exactly-once"))) { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await;
        worker.abort();
        let _ = worker.await;
        result.unwrap();
        assert!(h.fake.calls.lock().unwrap().iter().all(|(_, path, _)| !path.ends_with(":pull")));
    }

    #[tokio::test]
    async fn invalid_ack_drops_batch_and_repulls_new_id_without_repeating_turn() {
        let h = harness().await;
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        let mut waiting = e2e_seam::arm_waiting(h.home.path());
        let mut batch = VecDeque::new();
        h.fake.invalid_ack.store(true, Ordering::SeqCst);
        assert!(pull(&h, &mut batch).await.unwrap_err().contains("重新拉取"));
        assert!(batch.is_empty());
        let turn = tokio::time::timeout(Duration::from_secs(10), waiting.entered.recv()).await.unwrap().unwrap();
        *h.fake.delivery.lock().unwrap() = vec![delivery("fresh-ack", &notification())];
        pull(&h, &mut batch).await.unwrap();
        assert!(batch.is_empty());
        assert_eq!(acked(&h), vec![json!({"ackIds":["ack-one"]}), json!({"ackIds":["ack-fresh-ack"]})]);
        assert!(waiting.entered.try_recv().is_err());
        let calls = h.fake.calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|(_, path, _)| path.ends_with(":pull")).count(), 2);
        assert_eq!(calls.iter().filter(|(method, _, _)| *method == Method::GET).count(), 1);
        drop(calls);
        turn.release.send(()).unwrap();
    }

    #[tokio::test]
    async fn new_pubsub_id_and_ack_retry_do_not_repeat_running_or_done_resource() {
        let h = harness().await;
        let mut batch = VecDeque::new();
        pull(&h, &mut batch).await.unwrap();
        assert!(h.fake.calls.lock().unwrap().is_empty(), "non-holder cannot pull");
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        assert!(crate::googlechat::start_googlechat_webhook(h.home.path(), h.state.ctx.clone()).await.is_none());
        let mut waiting = e2e_seam::arm_waiting(h.home.path());
        h.fake.fail_ack.store(true, Ordering::SeqCst);
        assert!(pull(&h, &mut batch).await.is_err());
        assert_eq!(batch.len(), 1, "transient ACK failures retain their batch");
        let turn = tokio::time::timeout(Duration::from_secs(10), waiting.entered.recv()).await.unwrap().unwrap();
        pull(&h, &mut batch).await.unwrap();
        *h.fake.delivery.lock().unwrap() = vec![delivery("new-id", &notification())];
        pull(&h, &mut batch).await.unwrap();
        assert!(waiting.entered.try_recv().is_err());
        assert_eq!(acked(&h).last().unwrap(), &json!({"ackIds":["ack-new-id"]}));
        turn.release.send(()).unwrap();
        let key = event_key(&notification_reference(&notification()).unwrap());
        tokio::time::timeout(Duration::from_secs(10), async {
            while !h.inbox.transition(&stream(&h.state, &h.config), &key, "done", "done").unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        *h.fake.delivery.lock().unwrap() = vec![delivery("done-replay", &notification())];
        pull(&h, &mut batch).await.unwrap();
        assert!(waiting.entered.try_recv().is_err());
        assert_eq!(h.fake.calls.lock().unwrap().iter().filter(|(method, _, _)| *method == Method::GET).count(), 1);
    }

    #[tokio::test]
    async fn full_queue_acks_and_drops_without_later_turn() {
        let h = harness().await;
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        let mut waiting = e2e_seam::arm_waiting(h.home.path());
        let group = json!([Value::Null,"spaces/AAA"]).to_string();
        let (release, blocked) = tokio::sync::oneshot::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        h.state.ctx.googlechat_queue.submit_in(&group, "blocker", async move { let _ = blocked.await; let _ = done.send(()); }).unwrap();
        let mut batch = VecDeque::new();
        pull(&h, &mut batch).await.unwrap();
        assert_eq!(acked(&h), vec![json!({"ackIds":["ack-one"]})]);
        assert!(h.inbox.pending(&stream(&h.state, &h.config)).unwrap().is_empty());
        release.send(()).unwrap(); finished.await.unwrap();
        *h.fake.delivery.lock().unwrap() = vec![delivery("overload-replay", &notification())];
        pull(&h, &mut batch).await.unwrap();
        assert!(waiting.entered.try_recv().is_err());
        assert!(h.fake.calls.lock().unwrap().iter().all(|(_, path, _)| path != "/spaces/AAA/messages"));
    }

    #[tokio::test]
    async fn queued_reference_after_restart_reaches_real_processing_once() {
        let mut h = harness().await;
        let reference = notification_reference(&notification()).unwrap();
        let key = event_key(&reference);
        let stream = stream(&h.state, &h.config);
        h.inbox.persist(&stream, &key, &reference.to_string()).unwrap();
        h.inbox.transition(&stream, &key, "pending", "queued").unwrap();
        let other = tempfile::tempdir().unwrap();
        h.inbox = Inbox::shared(other.path()).unwrap();
        h.inbox = Inbox::shared(h.home.path()).unwrap();
        let mut waiting = e2e_seam::arm_waiting(h.home.path());
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        drain(&h.state, &h.config, &h.inbox).await.unwrap();
        let turn = tokio::time::timeout(Duration::from_secs(10), waiting.entered.recv()).await.unwrap().unwrap();
        drain(&h.state, &h.config, &h.inbox).await.unwrap();
        assert!(waiting.entered.try_recv().is_err());
        turn.release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !h.inbox.transition(&stream, &key, "done", "done").unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        drain(&h.state, &h.config, &h.inbox).await.unwrap();
        assert!(waiting.entered.try_recv().is_err());
    }

    #[tokio::test]
    async fn committed_reference_recovers_but_uncertain_replay_with_new_id_is_dropped() {
        let mut h = harness().await;
        let key = event_key(&notification_reference(&notification()).unwrap());
        let stream = stream(&h.state, &h.config);
        h.inbox.persist(&stream, &key, &notification_reference(&notification()).unwrap().to_string()).unwrap();
        h.inbox.transition(&stream, &key, "pending", "queued").unwrap();
        // Drop the shared connection to simulate a process restart before dispatch.
        let other = tempfile::tempdir().unwrap();
        h.inbox = Inbox::shared(other.path()).unwrap();
        h.inbox = Inbox::shared(h.home.path()).unwrap();
        assert_eq!(h.inbox.pending(&stream).unwrap().len(), 1);
        h.inbox.transition(&stream, &key, "pending", "running").unwrap();
        h.inbox = Inbox::shared(other.path()).unwrap();
        h.inbox = Inbox::shared(h.home.path()).unwrap();
        assert!(h.inbox.diagnostic(&stream).unwrap().is_some());
        let mut waiting = e2e_seam::arm_waiting(h.home.path());
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        *h.fake.delivery.lock().unwrap() = vec![delivery("new-uncertain-id", &notification())];
        pull(&h, &mut VecDeque::new()).await.unwrap();
        assert_eq!(acked(&h), vec![json!({"ackIds":["ack-new-uncertain-id"]})]);
        assert!(waiting.entered.try_recv().is_err());
        assert!(h.fake.calls.lock().unwrap().iter().all(|(method, _, _)| *method != Method::GET));
    }

    #[tokio::test]
    async fn connection_switch_and_invalid_config_fail_closed() {
        let h = harness().await;
        duduclaw_core::gateway_instance::acquire(h.home.path()).unwrap();
        let path = h.home.path().join("config.toml");
        let raw = tokio::fs::read_to_string(&path).await.unwrap();
        tokio::fs::write(&path, raw.replace("googlechat_connection = \"pubsub\"", "googlechat_connection = \"http\"")).await.unwrap();
        pull(&h, &mut VecDeque::new()).await.unwrap();
        assert!(h.fake.calls.lock().unwrap().is_empty());
        for raw in ["not toml", "[channels]\ngooglechat_connection = true", "[channels]\ngooglechat_connection = 'invalid'", "[channels]\ngooglechat_unknown = true", "[channels]\ngooglechat_pubsub_subscription = true", "[channels]\ngooglechat_pubsub_subscription = 'projects/test-project/subscriptions/chat-events'"] {
            tokio::fs::write(&path, raw).await.unwrap();
            assert!(!shared_http_enabled(h.home.path()).await);
        }
        tokio::fs::write(&path, "[channels]\n").await.unwrap();
        assert!(shared_http_enabled(h.home.path()).await);
    }
}
