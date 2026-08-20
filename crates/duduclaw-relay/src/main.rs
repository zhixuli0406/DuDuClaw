//! DuDuClaw Cloud Relay — standalone entrypoint.
//!
//! Terminates no TLS itself (Cloud Run, or any reverse proxy in front,
//! does that); binds plain HTTP/WS on `RELAY_BIND` (or Cloud Run's
//! injected `PORT`).

use std::net::SocketAddr;

use duduclaw_relay::config::RelayConfig;
use duduclaw_relay::db::RelayDb;
use duduclaw_relay::state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    let config = match RelayConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("duduclaw-relay: config error: {e}");
            std::process::exit(1);
        }
    };

    let db = match RelayDb::open(&config.db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("duduclaw-relay: failed to open device database: {e}");
            std::process::exit(1);
        }
    };

    let state = AppState::new(db);
    let app = duduclaw_relay::build_router(state);

    tracing::info!(
        bind = %config.bind,
        db_path = %config.db_path.display(),
        "starting duduclaw-relay"
    );

    let listener = match tokio::net::TcpListener::bind(config.bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("duduclaw-relay: failed to bind {}: {e}", config.bind);
            std::process::exit(1);
        }
    };

    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());

    if let Err(e) = serve.await {
        eprintln!("duduclaw-relay: server error: {e}");
        std::process::exit(1);
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining connections");
}
