//! Environment-driven configuration for the relay service.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Runtime configuration, resolved once at startup from environment
/// variables. Every field has a safe default so the service boots with zero
/// configuration in local development; a real deployment always sets
/// `RELAY_BIND` (or relies on Cloud Run's injected `PORT`) explicitly.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Address to bind the plain-HTTP listener on. No TLS is terminated
    /// here — Cloud Run (or any reverse proxy in front) is expected to
    /// terminate TLS and forward plain HTTP/WS to this port.
    pub bind: SocketAddr,
    /// SQLite database file path (WAL mode).
    pub db_path: PathBuf,
}

impl RelayConfig {
    /// Resolve configuration from environment variables.
    ///
    /// - `RELAY_BIND` — full `host:port` to bind (default `0.0.0.0:8080`).
    ///   Cloud Run injects `PORT`; when `RELAY_BIND` is unset but `PORT` is
    ///   present, that port is used on `0.0.0.0`.
    /// - `RELAY_DB_PATH` — SQLite file path (default `./relay.db`).
    pub fn from_env() -> Result<Self, String> {
        let bind_str = match std::env::var("RELAY_BIND") {
            Ok(v) => v,
            Err(_) => match std::env::var("PORT") {
                Ok(port) => format!("0.0.0.0:{port}"),
                Err(_) => "0.0.0.0:8080".to_string(),
            },
        };
        let bind: SocketAddr = bind_str
            .parse()
            .map_err(|e| format!("invalid RELAY_BIND/PORT address '{bind_str}': {e}"))?;

        let db_path: PathBuf = std::env::var("RELAY_DB_PATH")
            .unwrap_or_else(|_| "./relay.db".to_string())
            .into();

        Ok(Self { bind, db_path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `std::env` is process-global; Rust runs unit tests in parallel by
    // default, so tests that mutate env vars must serialize against each
    // other (a shared mutex) or they flake under `cargo test`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn defaults_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only env manipulation, serialized via ENV_LOCK.
        unsafe {
            std::env::remove_var("RELAY_BIND");
            std::env::remove_var("PORT");
            std::env::remove_var("RELAY_DB_PATH");
        }
        let cfg = RelayConfig::from_env().unwrap();
        assert_eq!(cfg.bind.port(), 8080);
        assert_eq!(cfg.db_path, PathBuf::from("./relay.db"));
    }

    #[test]
    fn port_env_used_when_bind_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("RELAY_BIND");
            std::env::set_var("PORT", "9999");
        }
        let cfg = RelayConfig::from_env().unwrap();
        assert_eq!(cfg.bind.port(), 9999);
        unsafe {
            std::env::remove_var("PORT");
        }
    }

    #[test]
    fn explicit_bind_wins_over_port() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("RELAY_BIND", "127.0.0.1:1234");
            std::env::set_var("PORT", "9999");
        }
        let cfg = RelayConfig::from_env().unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:1234".parse::<SocketAddr>().unwrap());
        unsafe {
            std::env::remove_var("RELAY_BIND");
            std::env::remove_var("PORT");
        }
    }

    #[test]
    fn invalid_bind_is_rejected() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("RELAY_BIND", "not-an-address");
        }
        assert!(RelayConfig::from_env().is_err());
        unsafe {
            std::env::remove_var("RELAY_BIND");
        }
    }
}
