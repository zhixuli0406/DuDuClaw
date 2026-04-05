use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::ConnectInfo,
    response::IntoResponse,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

static WS_RATE_LIMITER: std::sync::LazyLock<Mutex<HashMap<IpAddr, (Instant, u32)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn check_ws_rate_limit(ip: IpAddr) -> bool {
    let mut map = WS_RATE_LIMITER.lock().unwrap();
    let now = Instant::now();
    // Cleanup stale entries every time the map grows large
    if map.len() > 1000 {
        map.retain(|_, (t, _)| now.duration_since(*t).as_secs() < 120);
    }
    let entry = map.entry(ip).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() > 60 {
        *entry = (now, 1);
        return true;
    }
    entry.1 += 1;
    entry.1 <= 30 // max 30 WS connections per minute per IP
}

use crate::auth::AuthManager;
use crate::handlers::MethodHandler;
use crate::protocol::WsFrame;

/// Configuration for the WebSocket RPC gateway.
pub struct GatewayConfig {
    /// Bind address (e.g. `"0.0.0.0"`).
    pub bind: String,
    /// Port to listen on.
    pub port: u16,
    /// Optional authentication token.  When `None`, authentication is
    /// disabled.
    pub auth_token: Option<String>,
    /// Path to the DuDuClaw home directory (e.g. `~/.duduclaw`).
    pub home_dir: std::path::PathBuf,
}

/// Internal shared state for the Axum application.
struct AppState {
    auth: AuthManager,
    handler: MethodHandler,
    tx: broadcast::Sender<String>,
    /// Broadcast channel for real-time events (channel status, etc.) pushed to clients.
    event_tx: broadcast::Sender<String>,
}

/// Start the WebSocket RPC gateway and block until it shuts down.
pub async fn start_gateway(config: GatewayConfig) -> duduclaw_core::error::Result<()> {
    // Initialise the log broadcast channel (must happen before subscribers connect).
    let log_tx = crate::log::init_log_broadcaster();
    let tx = log_tx;

    let home_dir = config.home_dir.clone();
    let handler = MethodHandler::new(config.home_dir).await;

    // Initialize cost telemetry (must happen before any Claude CLI calls)
    if let Err(e) = crate::cost_telemetry::init_telemetry(&home_dir) {
        tracing::warn!(error = %e, "Failed to initialize cost telemetry — continuing without it");
    }

    // Initialize session manager
    let session_db_path = home_dir.join("sessions.db");
    let session_manager = Arc::new(
        crate::session::SessionManager::new(&session_db_path)
            .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(
                format!("Failed to initialize session manager: {e}")
            ))?,
    );

    // Start periodic session cleanup (every 6 hours, remove sessions older than 72 hours)
    {
        let sm = session_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
            loop {
                interval.tick().await;
                match sm.cleanup_inactive(72).await {
                    Ok(n) if n > 0 => info!("Cleaned up {} inactive sessions", n),
                    Ok(_) => {}
                    Err(e) => warn!("Session cleanup error: {}", e),
                }
            }
        });
    }

    // ── Cost telemetry: periodic cleanup + adaptive routing ────
    {
        let hd = home_dir.clone();
        tokio::spawn(async move {
            // Wait 10 minutes before first check
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                crate::cost_telemetry::adaptive_routing_check(&hd).await;
            }
        });
    }

    // ── Initialize prediction engine (Phase 1) ────────────────
    // Embedding provider: None for now (Tier 2 vocabulary_novelty fallback).
    // When BGE-small-zh is available at ~/.duduclaw/models/embedding/bge-small-zh/,
    // pass Some(Arc::new(OnnxEmbeddingProvider::load(...))) here.
    let prediction_db_path = home_dir.join("prediction.db");
    let metacognition_path = home_dir.join("metacognition.json");
    let prediction_engine = Arc::new(
        crate::prediction::engine::PredictionEngine::new(
            prediction_db_path,
            Some(metacognition_path.clone()),
        )
    );
    info!("Prediction engine initialized (embedding: none, using vocabulary_novelty fallback)");

    // ── Initialize GVU loop (Phase 2) ────────────────────────
    let gvu_db_path = home_dir.join("evolution.db");
    // Load encryption key for rollback_diff at rest (reuses existing keyfile)
    let gvu_encryption_key = crate::config_crypto::load_keyfile_public(&home_dir);
    let gvu_loop = Arc::new(crate::gvu::loop_::GvuLoop::with_encryption(
        &gvu_db_path,
        None, // observation_hours — will be set per-agent from config
        None, // max_generations — will be set per-agent from config
        gvu_encryption_key.as_ref(),
    ));
    info!("GVU evolution loop initialized (encryption: {})", if gvu_encryption_key.is_some() { "enabled" } else { "disabled" });

    // Event broadcast channel for pushing real-time updates (e.g. channel status) to dashboard
    let (event_tx, _) = broadcast::channel::<String>(64);

    // Start channel bots if configured
    let reply_ctx = Arc::new(
        crate::channel_reply::ReplyContext::new(
            handler.registry().clone(),
            home_dir.clone(),
            session_manager,
            handler.channel_status().clone(),
            event_tx.clone(),
        )
        .with_prediction_engine(prediction_engine.clone())
        .with_gvu_loop(gvu_loop.clone())
    );
    // Inject reply context into handler for channel hot-start/stop
    handler.set_reply_ctx(reply_ctx.clone()).await;

    // Store background task handles for graceful shutdown (BE-L4)
    let mut bg_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // Start channel bots — per-agent where supported
    for (label, h) in crate::telegram::start_telegram_bots(&home_dir, reply_ctx.clone()).await {
        handler.register_channel_handle(&label, h).await;
    }
    // Slack: per-agent support ready in slack.rs but module has unresolved
    // dependencies (shared_http_client, url crate). Slack bot started via
    // existing mechanism when those are resolved.
    // for (label, h) in crate::slack::start_slack_bots(&home_dir, reply_ctx.clone()).await {
    //     handler.register_channel_handle(&label, h).await;
    // }
    for (label, h) in crate::discord::start_discord_bots(&home_dir, reply_ctx.clone()).await {
        handler.register_channel_handle(&label, h).await;
    }
    // Webhook channels (LINE, WhatsApp, Feishu) — global only for now
    // Per-agent webhook routing requires multi-path routers (TODO-per-agent-channels.md)
    let line_router = crate::line::start_line_bot(&home_dir, reply_ctx.clone()).await;
    let webchat_ctx = reply_ctx.clone();

    // Start unified heartbeat scheduler (per-agent: evolution + cron + monitoring)
    // Replaces the old start_evolution_timers — each agent's HeartbeatConfig
    // now drives meso/macro reflections at its own interval or cron schedule.
    let heartbeat = duduclaw_agent::heartbeat::start_heartbeat_scheduler(
        home_dir.clone(),
        handler.registry().clone(),
    );
    handler.set_heartbeat(heartbeat).await;
    info!("Heartbeat scheduler started (per-agent evolution + monitoring)");

    // Start cron scheduler (reads cron_tasks.jsonl, fires on schedule)
    bg_handles.push(crate::cron_scheduler::start_cron_scheduler(
        home_dir.clone(),
        handler.registry().clone(),
    ));
    info!("Cron scheduler started");

    // Start agent dispatcher (consumes bus_queue.jsonl, spawns sub-agents)
    bg_handles.push(crate::dispatcher::start_agent_dispatcher(
        home_dir.clone(),
        handler.registry().clone(),
    ));
    info!("Agent dispatcher started ({} background tasks)", bg_handles.len());

    // ── Periodic update check (every 6 hours) — broadcast to dashboard ──
    {
        let etx = event_tx.clone();
        tokio::spawn(async move {
            // First check after 30 seconds (let gateway finish startup)
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            loop {
                match crate::updater::check_update().await {
                    Ok(info) if info.available => {
                        let event = WsFrame::Event {
                            event: "system.update_available".to_string(),
                            payload: serde_json::json!({
                                "available": true,
                                "current_version": info.current_version,
                                "latest_version": info.latest_version,
                                "release_notes": info.release_notes,
                                "published_at": info.published_at,
                                "install_method": info.install_method,
                            }),
                            seq: None,
                            state_version: None,
                        };
                        if let Ok(json) = serde_json::to_string(&event) {
                            let _ = etx.send(json);
                        }
                        info!(
                            latest = %info.latest_version,
                            "New version available — notified dashboard clients"
                        );
                    }
                    Ok(_) => { /* up to date, no broadcast */ }
                    Err(e) => {
                        tracing::debug!(error = %e, "Periodic update check failed (will retry)");
                    }
                }
                // Check every 6 hours
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            }
        });
        info!("Periodic update checker started (every 6h)");
    }

    let state = Arc::new(AppState {
        auth: AuthManager::new(config.auth_token),
        handler,
        tx,
        event_tx,
    });

    // WebChat endpoint — separate state from main /ws (different auth model)
    let webchat_state = Arc::new(crate::webchat::WebChatState::new(webchat_ctx));
    let webchat_router = Router::new()
        .route("/ws/chat", get(crate::webchat::ws_chat_handler))
        .with_state(webchat_state);

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .with_state(state)
        .merge(webchat_router);

    // Mount LINE webhook endpoint
    if let Some(line) = line_router {
        app = app.merge(line);
    }

    #[cfg(feature = "dashboard")]
    {
        app = app.merge(duduclaw_dashboard::dashboard_router());
    }

    let app = app;

    let addr = format!("{}:{}", config.bind, config.port);
    info!("Gateway starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

    // Serve with graceful shutdown on Ctrl+C
    let pe_for_shutdown = prediction_engine.clone();
    let meta_path_for_shutdown = metacognition_path.clone();
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Shutdown signal received, flushing state...");
            pe_for_shutdown.flush_all().await;
            pe_for_shutdown.persist_metacognition(&meta_path_for_shutdown).await;
            info!("Prediction engine state flushed");
        })
        .await
        .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

    Ok(())
}

/// Axum handler that upgrades HTTP to WebSocket.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    // Rate limit: max 30 WS connections per minute per IP.
    if !check_ws_rate_limit(addr.ip()) {
        warn!(ip = %addr.ip(), "WebSocket connection rejected: rate limit exceeded");
        return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    // Validate Origin header to prevent cross-site WebSocket hijacking.
    // Non-browser clients (curl, SDK) don't send Origin, so absent is OK.
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        let is_safe = origin.starts_with("http://127.0.0.1")
            || origin.starts_with("http://localhost")
            || origin.starts_with("https://127.0.0.1")
            || origin.starts_with("https://localhost");
        if !is_safe {
            warn!(origin, "WebSocket connection rejected: invalid origin");
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
    }
    ws.max_message_size(1024 * 1024) // 1MB max WebSocket message
      .on_upgrade(move |socket| handle_socket(socket, state))
}

/// Process a single WebSocket connection.
async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    info!("New WebSocket connection established");

    // --- Authentication gate ---
    // If auth is required, the first message MUST be a "connect" request
    // carrying a valid token. Reject and close otherwise.
    if state.auth.is_auth_required() {
        // Timeout auth handshake to prevent Slowloris-style resource exhaustion (BE-C4)
        let auth_timeout = std::time::Duration::from_secs(10);
        let authenticated = match tokio::time::timeout(auth_timeout, socket.recv()).await {
            Err(_) => {
                warn!("WebSocket auth timeout — closing connection");
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
            Ok(recv_result) => match recv_result {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<WsFrame>(&text) {
                    Ok(WsFrame::Request { id, method, params }) if method == "connect" => {
                        // ── Ed25519 challenge-response ──────────────────────
                        // If the client sends `{ "method": "connect", "params": {} }`
                        // (no token) and Ed25519 is configured, issue a challenge.
                        // The client must then send `{ "method": "authenticate",
                        // "params": { "signature": "<b64>" } }`.
                        if state.auth.is_ed25519() {
                            let challenge = state.auth.issue_challenge();
                            let resp = WsFrame::ok_response(
                                &id,
                                serde_json::json!({ "challenge": challenge }),
                            );
                            let _ = socket.send(Message::Text(
                                serde_json::to_string(&resp).unwrap_or_default().into(),
                            )).await;

                            // Wait for the `authenticate` message (with timeout)
                            match tokio::time::timeout(auth_timeout, socket.recv()).await.unwrap_or(None) {
                                Some(Ok(Message::Text(auth_text))) => {
                                    match serde_json::from_str::<WsFrame>(&auth_text) {
                                        Ok(WsFrame::Request { id: auth_id, method: auth_method, params: auth_params })
                                            if auth_method == "authenticate" =>
                                        {
                                            let sig = auth_params
                                                .get("signature")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            match state.auth.verify_ed25519(sig) {
                                                Ok(()) => {
                                                    let ok = WsFrame::ok_response(
                                                        &auth_id,
                                                        serde_json::json!({"status": "authenticated"}),
                                                    );
                                                    let _ = socket.send(Message::Text(
                                                        serde_json::to_string(&ok).unwrap_or_default().into(),
                                                    )).await;
                                                    true
                                                }
                                                Err(_) => {
                                                    let err = WsFrame::error_response(&auth_id, "Ed25519 authentication failed");
                                                    let _ = socket.send(Message::Text(
                                                        serde_json::to_string(&err).unwrap_or_default().into(),
                                                    )).await;
                                                    false
                                                }
                                            }
                                        }
                                        _ => {
                                            let err = WsFrame::error_response("", "expected authenticate message");
                                            let _ = socket.send(Message::Text(
                                                serde_json::to_string(&err).unwrap_or_default().into(),
                                            )).await;
                                            false
                                        }
                                    }
                                }
                                _ => false,
                            }
                        } else {
                            // ── Token authentication ────────────────────────
                            let token = params.get("token").and_then(|v| v.as_str()).unwrap_or("");
                            match state.auth.validate(token) {
                                Ok(()) => {
                                    let ok = WsFrame::ok_response(
                                        &id,
                                        serde_json::json!({"status": "authenticated"}),
                                    );
                                    let _ = socket.send(Message::Text(
                                        serde_json::to_string(&ok).unwrap_or_default().into(),
                                    )).await;
                                    true
                                }
                                Err(_) => {
                                    let err = WsFrame::error_response(&id, "authentication failed");
                                    let _ = socket.send(Message::Text(
                                        serde_json::to_string(&err).unwrap_or_default().into(),
                                    )).await;
                                    false
                                }
                            }
                        }
                    }
                    _ => {
                        let err = WsFrame::error_response("", "expected connect message with token");
                        let _ = socket.send(Message::Text(
                            serde_json::to_string(&err).unwrap_or_default().into(),
                        )).await;
                        false
                    }
                }
            }
            _ => false,
        } // match recv_result
        }; // match tokio::time::timeout

        if !authenticated {
            warn!("WebSocket auth failed – closing connection");
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    }

    // Split the socket so we can drive sending and receiving concurrently.
    let (mut sink, mut stream) = socket.split();
    let mut log_rx = state.tx.subscribe();
    let mut event_rx = state.event_tx.subscribe();
    let mut logs_subscribed = false;

    loop {
        tokio::select! {
            // ── Incoming WebSocket frames ───────────────────
            msg_opt = stream.next() => {
                let msg = match msg_opt {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => { warn!("WebSocket receive error: {e}"); break; }
                    None => break,
                };

                match msg {
                    Message::Text(text) => {
                        let frame = match serde_json::from_str::<WsFrame>(&text) {
                            Ok(f) => f,
                            Err(e) => {
                                error!("Failed to parse WsFrame: {e}");
                                let err_resp = WsFrame::error_response("", "invalid frame");
                                let resp_text = serde_json::to_string(&err_resp).unwrap_or_default();
                                if sink.send(Message::Text(resp_text.into())).await.is_err() { break; }
                                continue;
                            }
                        };

                        match frame {
                            WsFrame::Request { id, method, params } => {
                                // Track log subscription state
                                if method == "logs.subscribe" {
                                    logs_subscribed = true;
                                } else if method == "logs.unsubscribe" {
                                    logs_subscribed = false;
                                }

                                let mut response = state.handler.handle(&method, params).await;
                                if let WsFrame::Response { id: ref mut resp_id, .. } = response {
                                    *resp_id = id;
                                }
                                let resp_text = serde_json::to_string(&response).unwrap_or_default();
                                if sink.send(Message::Text(resp_text.into())).await.is_err() { break; }
                            }
                            other => { warn!("Received non-request frame: {:?}", other); }
                        }
                    }
                    Message::Close(_) => { info!("WebSocket connection closed by client"); break; }
                    Message::Ping(data) => {
                        if sink.send(Message::Pong(data)).await.is_err() { break; }
                    }
                    _ => {}
                }
            }

            // ── Outbound log broadcast (only when subscribed) ─
            log_line = log_rx.recv(), if logs_subscribed => {
                match log_line {
                    Ok(line) => {
                        // Send as WsFrame::Event so the frontend can parse it uniformly
                        let data = serde_json::from_str::<serde_json::Value>(&line)
                            .unwrap_or(serde_json::Value::String(line));
                        let push = WsFrame::Event {
                            event: "logs.entry".to_string(),
                            payload: data,
                            seq: None,
                            state_version: None,
                        };
                        let text = serde_json::to_string(&push).unwrap_or_default();
                        if sink.send(Message::Text(text.into())).await.is_err() { break; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {} // drop missed events
                    Err(_) => break,
                }
            }

            // ── Outbound event broadcast (always active for authenticated clients) ─
            event_line = event_rx.recv() => {
                match event_line {
                    Ok(json) => {
                        // Events are already serialized as WsFrame::Event JSON
                        if sink.send(Message::Text(json.into())).await.is_err() { break; }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {} // drop missed events
                    Err(_) => break,
                }
            }
        }
    }

    info!("WebSocket connection terminated");
}

/// Simple health-check endpoint.
async fn health_handler() -> &'static str {
    "ok"
}
