//! `duduclaw-cli-worker` binary entrypoint.
//!
//! Boots a [`WorkerServer`] bound to `127.0.0.1` and serves until SIGTERM
//! (or Ctrl-C). The bearer token is read from env var
//! `DUDUCLAW_WORKER_TOKEN`, then auto-generated to
//! `<home>/cli-worker.token` if absent.
//!
//! Round 4 deferred-cleanup (LOW-2): the previous `--token` CLI flag is
//! gone. Passing secrets as CLI args is unsafe on multi-user hosts —
//! every local user can read it via `ps -ef` / `/proc/<pid>/cmdline`.
//! The supervisor already passes the token through
//! `DUDUCLAW_WORKER_TOKEN`, so dropping the flag has no operational
//! impact on the managed-worker path; manual launches that previously
//! used `--token foo` should switch to `DUDUCLAW_WORKER_TOKEN=foo`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use duduclaw_cli_worker::{TokenStore, WorkerServer, WorkerServerConfig};
#[allow(unused_imports)] // `warn` is only used on cfg(windows)
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "duduclaw-cli-worker",
    version,
    about = "Localhost PTY-pool worker for DuDuClaw"
)]
struct Cli {
    /// Address to bind. MUST be a loopback (127.0.0.1 or ::1).
    #[arg(long, default_value = "127.0.0.1:9876")]
    bind: SocketAddr,

    /// DuDuClaw home directory (where `cli-worker.token` lives).
    #[arg(long, default_value = "~/.duduclaw")]
    home_dir: PathBuf,

    /// Max concurrent invokes per pooled agent. CLI binaries don't support
    /// re-entrancy on a single TUI; leave at 1 unless you know what you're doing.
    #[arg(long, default_value_t = 1)]
    max_per_agent: usize,

    /// Idle session eviction window, in seconds.
    #[arg(long, default_value_t = 600)]
    idle_timeout_secs: u64,

    /// Default per-invoke timeout, in seconds. Overridable per request
    /// via `timeout_ms`.
    #[arg(long, default_value_t = 300)]
    invoke_timeout_secs: u64,

    /// **Review fix**: default `claude --model X` for sessions that
    /// don't carry an explicit per-Invoke `model` in their RPC. Omit
    /// to let the CLI pick its built-in default.
    #[arg(long)]
    default_model: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let home_dir = expand_home(&cli.home_dir)?;

    let token = resolve_token(&home_dir)?;
    info!(
        bind = %cli.bind,
        home = %home_dir.display(),
        token_chars = token.len(),
        "worker: starting"
    );

    let config = WorkerServerConfig {
        bind: cli.bind,
        token,
        max_per_agent: cli.max_per_agent,
        idle_timeout: Duration::from_secs(cli.idle_timeout_secs),
        default_invoke_timeout: Duration::from_secs(cli.invoke_timeout_secs),
        default_model: cli.default_model.clone(),
        home_dir: Some(home_dir.clone()),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    let server = WorkerServer::new(config)?;
    server.serve(shutdown_signal()).await?;
    info!("worker: clean shutdown");
    Ok(())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Expand a leading `~/` to the user's home directory.
///
/// **Review fix (L20)**: when the path begins with `~/` but `$HOME`
/// cannot be resolved, return a clear error instead of leaking the
/// literal `~/.duduclaw` path downstream. The previous code returned
/// the un-expanded `~` path, which made `work_dir` validation reject
/// every request with a misleading "must canonicalize inside
/// <home>/agents/" message that pointed at a non-existent `~` dir.
fn expand_home(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    if let Some(stripped) = path.to_str().and_then(|s| s.strip_prefix("~/")) {
        match dirs::home_dir() {
            Some(home) => return Ok(home.join(stripped)),
            None => {
                anyhow::bail!(
                    "home directory contains a leading `~` but $HOME could not be resolved; \
                     pass an absolute --home-dir instead (got {})",
                    path.display()
                );
            }
        }
    }
    Ok(path.to_path_buf())
}

fn resolve_token(home: &std::path::Path) -> anyhow::Result<String> {
    if let Ok(env_token) = std::env::var("DUDUCLAW_WORKER_TOKEN") {
        if !env_token.is_empty() {
            return Ok(env_token);
        }
    }
    let store = TokenStore::new(home);
    let token = store.load_or_generate()?;
    info!(path = %store.path().display(), "worker: using token file");
    Ok(token)
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    tokio::select! {
        _ = sigterm.recv() => info!("worker: SIGTERM received"),
        _ = sigint.recv() => info!("worker: SIGINT received"),
    }
}

#[cfg(windows)]
async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        warn!(error = %e, "worker: ctrl_c handler failed");
    } else {
        info!("worker: Ctrl-C received");
    }
}

// Silence the unused-import warning on the non-target platform.
#[cfg(not(any(unix, windows)))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn expand_home_strips_tilde() {
        // Only meaningful when $HOME resolves (always true in CI test envs).
        if dirs::home_dir().is_none() {
            return;
        }
        let expanded = expand_home(Path::new("~/.duduclaw")).expect("home should resolve");
        assert!(!expanded.starts_with("~"));
        assert!(expanded.to_string_lossy().ends_with(".duduclaw"));
    }

    #[test]
    fn expand_home_passes_through_absolute() {
        let p = Path::new("/tmp/explicit");
        assert_eq!(expand_home(p).unwrap(), PathBuf::from("/tmp/explicit"));
    }

    #[test]
    fn expand_home_errors_when_home_unresolvable() {
        // **Review fix (L20)**: a `~/`-prefixed path with no resolvable
        // HOME must error, not silently return the literal `~` path.
        // Simulate an unresolvable HOME by clearing it for this test.
        // Note: this is best-effort — if the platform sources HOME from
        // elsewhere (e.g. passwd), `dirs::home_dir()` may still resolve,
        // in which case we just assert the happy path holds.
        let saved = std::env::var_os("HOME");
        unsafe {
            std::env::remove_var("HOME");
        }
        let result = expand_home(Path::new("~/.duduclaw"));
        // Restore before asserting so a panic doesn't leak env state.
        if let Some(v) = saved {
            unsafe {
                std::env::set_var("HOME", v);
            }
        }
        match dirs::home_dir() {
            None => assert!(result.is_err(), "expected error when HOME unresolvable"),
            Some(_) => assert!(result.is_ok(), "HOME still resolved; happy path holds"),
        }
    }
}
