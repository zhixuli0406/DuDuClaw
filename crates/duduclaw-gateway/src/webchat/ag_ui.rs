//! Authenticated AG-UI HTTP/SSE adapter over the guarded WebChat reply pipeline.
use super::*;
use axum::{extract::{DefaultBodyLimit, Path}, http::StatusCode, response::{Response, Sse, sse::{Event, KeepAlive}}, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{collections::HashMap, convert::Infallible, sync::{Mutex, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::sync::{mpsc, oneshot};

mod delivery;
mod input;
mod run;
mod store;
#[cfg(test)]
mod tests;
use input::{RunInput, ResumeEntry};

const MAX_BODY: usize = 256 * 1024;
const MAX_RUNS: usize = 10;
const MAX_PER_USER: usize = 2;
const RUN_LIFETIME: Duration = Duration::from_secs(15 * 60);
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) struct AgUiState {
    chat: Arc<WebChatState>,
    journal: Option<store::Store>,
    admission: tokio::sync::Mutex<()>,
    connections: Mutex<HashMap<String, usize>>,
    runs: Mutex<HashMap<String, Entry>>,
    receipts: Mutex<HashMap<String, (std::time::Instant, Vec<ResumeEntry>)>>,
}
struct Entry {
    user: String,
    resume: mpsc::Sender<Connection>,
    waiting: Arc<AtomicBool>,
}
struct Connection {
    input: RunInput,
    claims: duduclaw_auth::jwt::Claims,
    output: mpsc::Sender<Frame>,
}
struct Frame { value: Value, ack: oneshot::Sender<()>, permit: Option<delivery::Permit> }
impl Frame {
    fn event(&self) -> Result<Event, ()> {
        let event = || Event::default().data(self.value.to_string());
        match &self.permit { Some(permit) => permit.with_current(event), None => Ok(event()) }
    }
}
struct StreamLease { state: Arc<AgUiState>, user: String }
impl Drop for StreamLease {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.state.connections.lock() {
            if let Some(n) = counts.get_mut(&self.user) {
                *n = n.saturating_sub(1);
                if *n == 0 { counts.remove(&self.user); }
            }
        }
    }
}
struct RunLease { state: Arc<AgUiState>, sid: String }
impl Drop for RunLease {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.state.runs.lock() { runs.remove(&self.sid); }
    }
}

pub(crate) fn router(chat: Arc<WebChatState>) -> Router {
    // Router construction runs during gateway boot, before serving traffic.
    let journal = store::Store::open(&chat.ctx.home_dir).map_err(|error| {
        tracing::warn!(%error, "AG-UI journal unavailable");
    }).ok();
    Router::new().route("/api/ag-ui/agents/{agent_id}/runs", post(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(Arc::new(AgUiState { chat, journal, admission: tokio::sync::Mutex::new(()), connections: Mutex::new(HashMap::new()), runs: Mutex::new(HashMap::new()), receipts: Mutex::new(HashMap::new()) }))
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error":code}))).into_response()
}

fn session_id(user: &str, agent: &str, thread: &str) -> String {
    use sha2::{Digest, Sha256};
    // Length-delimited JSON, full hashes, and a separate namespace prevent
    // thread strings from addressing WebChat, other people or other employees.
    let key = serde_json::to_vec(&(user, agent, thread)).expect("string tuple");
    format!("webchat:webchat:{}:ag-ui:{}", webchat_owner_tag(user), hex::encode(Sha256::digest(key)))
}

async fn authenticate(state: &WebChatState, headers: &HeaderMap) -> Result<duduclaw_auth::jwt::Claims, Response> {
    let token = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ")).filter(|v| !v.is_empty())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "authentication_required"))?;
    // Never accept widget credentials or the local-session implicit authority.
    let claims = state.jwt_config.verify_access_token(token)
        .map_err(|_| error(StatusCode::UNAUTHORIZED, "invalid_credential"))?;
    let (_, password_change) = authenticate_with(&state.jwt_config, &state.user_db, token)
        .map_err(|_| error(StatusCode::UNAUTHORIZED, "invalid_credential"))?;
    if password_change { return Err(error(StatusCode::FORBIDDEN, "password_change_required")); }
    if state.credential_validity(Some(&claims), true).await != crate::handlers::live_context::SocketValidity::Valid {
        return Err(error(StatusCode::UNAUTHORIZED, "invalid_credential"));
    }
    Ok(claims)
}

async fn handle(State(state): State<Arc<AgUiState>>, Path(agent): Path<String>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let claims = match authenticate(&state.chat, &headers).await { Ok(c) => c, Err(r) => return r };
    if headers.get(axum::http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next()).map(str::trim) != Some("application/json") {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
    }
    let user = claims.sub.clone();
    {
        let mut counts = match state.connections.lock() { Ok(c) => c, Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable") };
        if counts.values().sum::<usize>() >= MAX_RUNS || counts.get(&user).copied().unwrap_or(0) >= MAX_PER_USER {
            return error(StatusCode::TOO_MANY_REQUESTS, "connection_limit");
        }
        *counts.entry(user.clone()).or_default() += 1;
    }
    let stream_lease = StreamLease { state: state.clone(), user: user.clone() };
    let input: RunInput = match serde_json::from_slice(&body) {
        Ok(input) => input,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_run_input"),
    };
    let text = match input.validate() { Ok(t) => t, Err(code) => return error(StatusCode::BAD_REQUEST, code) };
    if state.chat.authorize_target(&user, &agent, false).await.is_err() {
        return error(StatusCode::FORBIDDEN, "agent_unavailable");
    }
    if !state.chat.ctx.registry.read().await.get(&agent).is_some_and(|a| a.config.agent.status.is_operational()) {
        return error(StatusCode::FORBIDDEN, "agent_unavailable");
    }
    let Some(broker) = state.chat.approvals.clone() else { return error(StatusCode::SERVICE_UNAVAILABLE, "approvals_unavailable") };
    let principal = duduclaw_core::HumanPrincipal::user(&user).map(|p| p.to_string());
    let Some(principal) = principal else { return error(StatusCode::UNAUTHORIZED, "invalid_credential") };
    if broker.check_webchat_turn_admission(&principal).await.is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "approval_capacity");
    }
    let sid = session_id(&user, &agent, &input.thread_id);
    let _admission = state.admission.lock().await;
    let has_live_thread = match state.runs.lock() {
        Ok(runs) => runs.contains_key(&sid),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable"),
    };
    // Unknown resumes cannot allocate durable replay rows. Matching receipts
    // still pass through the principal quota and durable run-id deduplication.
    let replay = if !has_live_thread && !input.resume.is_empty() {
        let receipts = match state.receipts.lock() {
            Ok(receipts) => receipts,
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable"),
        };
        let matches = receipts.get(&sid).is_some_and(|(at, entries)|
            at.elapsed() < RUN_LIFETIME && same_decisions(entries, &input.resume));
        if !matches { return error(StatusCode::CONFLICT, "stale_or_unknown_interrupt"); }
        true
    } else { false };
    let Some(journal) = state.journal.as_ref() else { return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable") };
    if !has_live_thread {
        let expired = match journal.expired_turn(&sid).await {
            Ok(turn) => turn,
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable"),
        };
        if let Some(old_turn) = expired {
            if broker.withdraw_turn_for_principal(&sid, &old_turn, Some(&principal)).await.is_err()
                || journal.finish(&sid, &old_turn).await.is_err() {
                return error(StatusCode::SERVICE_UNAVAILABLE, "orphan_cleanup_failed");
            }
        }
        if input.resume.is_empty() {
            let runs = match state.runs.lock() { Ok(r) => r, Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable") };
            if runs.len() >= MAX_RUNS || runs.values().filter(|e| e.user == user).count() >= MAX_PER_USER {
                return error(StatusCode::TOO_MANY_REQUESTS, "run_limit");
            }
        }
    }
    let turn = match journal.admit(&principal, &sid, &input.run_id, has_live_thread).await {
        Ok(turn) => turn,
        Err(reason) => {
            tracing::debug!(%reason, "AG-UI admission refused");
            if matches!(reason.as_str(), "principal_journal_full" | "principal_rate_limit") {
                return error(StatusCode::TOO_MANY_REQUESTS, "principal_admission_limit");
            }
            return error(StatusCode::CONFLICT, "run_not_admitted");
        }
    };
    if replay {
        if journal.finish(&sid, &turn).await.is_err() { return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable"); }
    }
    let (output, receiver) = mpsc::channel(8);
    let connection = Connection { input, claims, output };
    let mut runs = match state.runs.lock() { Ok(r) => r, Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "run_store_unavailable") };
    // The old actor may finish while admission performs its SQLite await.
    // Never start another actor with that actor's host turn identity.
    if has_live_thread && !runs.contains_key(&sid) {
        return error(StatusCode::CONFLICT, "thread_changed");
    }
    if let Some(entry) = runs.get(&sid) {
        if connection.input.resume.is_empty() || !entry.waiting.swap(false, Ordering::AcqRel) {
            tokio::spawn(run::reject(connection, "thread_busy_or_resume_required"));
        } else if let Err(e) = entry.resume.try_send(connection) {
            entry.waiting.store(true, Ordering::Release);
            tokio::spawn(run::reject(e.into_inner(), "resume_unavailable"));
        }
    } else if !connection.input.resume.is_empty() {
        tokio::spawn(run::replay(connection));
    } else {
        let Some(text) = text else { return error(StatusCode::BAD_REQUEST, "user_message_required") };
        let (resume, incoming) = mpsc::channel(1);
        let waiting = Arc::new(AtomicBool::new(false));
        runs.insert(sid.clone(), Entry { user: user.clone(), resume, waiting: waiting.clone() });
        let lease = RunLease { state: state.clone(), sid: sid.clone() };
        tokio::spawn(run::start(state.clone(), broker, lease, user, agent, sid, turn, principal, text, connection, incoming, waiting));
    }
    drop(runs);
    // Acknowledgement happens when axum asks for the next frame. Dropping the
    // body drops the receiver and cancels an active run, including slow readers.
    let stream = futures_util::stream::unfold((receiver, None::<oneshot::Sender<()>>, stream_lease), |(mut receiver, ack, lease)| async move {
        if let Some(ack) = ack { let _ = ack.send(()); }
        let frame = receiver.recv().await?;
        // Check below the channel buffer, at the SSE body emission boundary.
        // A refused frame drops its acknowledgement and terminates the stream.
        let event = frame.event().ok()?;
        Some((Ok::<_, Infallible>(event), (receiver, Some(frame.ack), lease)))
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keep-alive")).into_response()
}

async fn send(connection: &Connection, value: Value) -> Result<(), ()> {
    send_frame(connection, value, None).await
}

async fn send_frame(connection: &Connection, value: Value, permit: Option<delivery::Permit>) -> Result<(), ()> {
    let (ack, received) = oneshot::channel();
    tokio::time::timeout(SEND_TIMEOUT, async {
        connection.output.send(Frame { value, ack, permit }).await.map_err(|_| ())?;
        received.await.map_err(|_| ())
    }).await.map_err(|_| ())?
}

fn same_decisions(a: &[ResumeEntry], b: &[ResumeEntry]) -> bool {
    a.len() == b.len() && a.iter().all(|entry| b.iter().any(|other|
        entry.interrupt_id == other.interrupt_id && entry.status == other.status && entry.payload == other.payload))
}

pub(super) fn is_session(sid: &str) -> bool {
    let mut parts = sid.split(':');
    parts.next() == Some("webchat") && parts.next() == Some("webchat")
        && parts.next().is_some() && parts.next() == Some("ag-ui")
}
