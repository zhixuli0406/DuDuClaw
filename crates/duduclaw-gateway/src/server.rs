use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

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
}

/// Start the WebSocket RPC gateway and block until it shuts down.
pub async fn start_gateway(config: GatewayConfig) -> duduclaw_core::error::Result<()> {
    let (tx, _rx) = broadcast::channel::<String>(100);

    let state = Arc::new(AppState {
        auth: AuthManager::new(config.auth_token),
        handler: MethodHandler::new(config.home_dir).await,
        tx,
    });

    #[allow(unused_mut)]
    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health_handler))
        .with_state(state);

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

    axum::serve(listener, app)
        .await
        .map_err(|e| duduclaw_core::error::DuDuClawError::Gateway(e.to_string()))?;

    Ok(())
}

/// Axum handler that upgrades HTTP to WebSocket.
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Process a single WebSocket connection.
async fn handle_socket(mut socket: WebSocket, state: Arc<AppState>) {
    info!("New WebSocket connection established");

    // --- Authentication gate ---
    // If auth is required, the first message MUST be a "connect" request
    // carrying a valid token. Reject and close otherwise.
    if state.auth.is_auth_required() {
        let authenticated = match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<WsFrame>(&text) {
                    Ok(WsFrame::Request { id, method, params }) if method == "connect" => {
                        let token = params
                            .get("token")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        match state.auth.validate(token) {
                            Ok(()) => {
                                let ok = WsFrame::ok_response(
                                    &id,
                                    serde_json::json!({"status": "authenticated"}),
                                );
                                let _ = socket
                                    .send(Message::Text(
                                        serde_json::to_string(&ok).unwrap_or_default().into(),
                                    ))
                                    .await;
                                true
                            }
                            Err(_) => {
                                let err = WsFrame::error_response(&id, "authentication failed");
                                let _ = socket
                                    .send(Message::Text(
                                        serde_json::to_string(&err).unwrap_or_default().into(),
                                    ))
                                    .await;
                                false
                            }
                        }
                    }
                    _ => {
                        let err =
                            WsFrame::error_response("", "expected connect message with token");
                        let _ = socket
                            .send(Message::Text(
                                serde_json::to_string(&err).unwrap_or_default().into(),
                            ))
                            .await;
                        false
                    }
                }
            }
            _ => false,
        };

        if !authenticated {
            warn!("WebSocket auth failed – closing connection");
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    }

    // Broadcast subscription (kept alive for the duration of the connection)
    let _rx = state.tx.subscribe();

    while let Some(msg_result) = socket.recv().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                warn!("WebSocket receive error: {}", e);
                break;
            }
        };

        match msg {
            Message::Text(text) => {
                let frame = match serde_json::from_str::<WsFrame>(&text) {
                    Ok(f) => f,
                    Err(e) => {
                        error!("Failed to parse WsFrame: {}", e);
                        let err_resp = WsFrame::error_response("", "invalid frame");
                        let resp_text = serde_json::to_string(&err_resp)
                            .unwrap_or_default();
                        if socket.send(Message::Text(resp_text.into())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                };

                match frame {
                    WsFrame::Request { id, method, params } => {
                        let mut response = state.handler.handle(&method, params).await;

                        // Patch the response id to match the request.
                        if let WsFrame::Response {
                            id: ref mut resp_id,
                            ..
                        } = response
                        {
                            *resp_id = id;
                        }

                        let resp_text =
                            serde_json::to_string(&response).unwrap_or_default();
                        if socket.send(Message::Text(resp_text.into())).await.is_err() {
                            break;
                        }
                    }
                    other => {
                        warn!("Received non-request frame: {:?}", other);
                    }
                }
            }
            Message::Close(_) => {
                info!("WebSocket connection closed by client");
                break;
            }
            Message::Ping(data) => {
                if socket.send(Message::Pong(data)).await.is_err() {
                    break;
                }
            }
            _ => {}
        }
    }

    info!("WebSocket connection terminated");
}

/// Simple health-check endpoint.
async fn health_handler() -> &'static str {
    "ok"
}
