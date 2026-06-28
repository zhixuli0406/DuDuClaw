//! Interactive CLI authentication — **"Dashboard 一鍵登入" for every AI CLI**.
//!
//! Each AI CLI (Claude / Codex / Gemini / Antigravity) ships its own native
//! login command. This module drives that command inside a PTY, streams its
//! output to the dashboard, and relays the user's input (e.g. a pasted
//! verification code) back. On success the CLI persists credentials to its own
//! store; DuDuClaw then detects / registers the account.
//!
//! ## Feasibility constraint (READ THIS)
//!
//! OAuth flows split into two kinds, with very different remote behaviour:
//!
//! - **device-code / paste-back** (`remote_safe = true`): the CLI prints a URL
//!   + code; the user approves in *their own* browser and pastes a code back.
//!   Works whether the dashboard is local or remote (Cloud).
//! - **localhost-callback** (`remote_safe = false`): the CLI opens a browser
//!   and waits for a redirect to `localhost:<port>` that *it* is listening on.
//!   This only completes when the dashboard runs on the **same machine** as the
//!   user's browser (self-host). For a remote Cloud dashboard it cannot finish
//!   — those CLIs must fall back to an API key.
//!
//! The per-CLI [`CliAuthSpec`] records `remote_safe` so the dashboard can warn
//! before starting a flow that can't complete remotely. Markers are best-effort
//! and tunable against live CLIs — they are intentionally centralised here.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::broadcast;

use duduclaw_core::types::RuntimeType;

/// One CLI's native login flow.
#[derive(Debug, Clone)]
pub struct CliAuthSpec {
    pub runtime: RuntimeType,
    /// Args appended to the resolved program (e.g. `["setup-token"]`).
    pub login_args: Vec<String>,
    /// Lowercase substrings that signal success.
    pub success_markers: Vec<String>,
    /// Lowercase substrings that signal failure.
    pub failure_markers: Vec<String>,
    /// `true` ⇒ device-code / paste-back ⇒ safe to drive for a remote dashboard.
    /// `false` ⇒ localhost-callback ⇒ only works when dashboard + browser share
    /// a machine (self-host).
    pub remote_safe: bool,
    /// Short zh-TW hint shown in the dashboard.
    pub hint: &'static str,
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Login spec for a runtime, or `None` for runtimes with no interactive login
/// (OpenAI-compat is API-key only).
pub fn spec_for(runtime: RuntimeType) -> Option<CliAuthSpec> {
    match runtime {
        RuntimeType::Claude => Some(CliAuthSpec {
            runtime,
            login_args: v(&["setup-token"]),
            // Broad, case-insensitive (tail is lowercased): `claude setup-token`
            // confirms with phrasings like "Success!", "Token has been saved",
            // "credentials saved", "you can now use" — narrow markers like
            // "successfully" / "token saved" miss those and leave the dashboard
            // spinning forever after a valid paste-back.
            success_markers: v(&["success", "authenticated", "logged in", "saved", "you can now"]),
            failure_markers: v(&["authentication failed", "login failed", "invalid code", "access denied", "token expired"]),
            // `claude setup-token` is the headless long-lived-token flow (paste-back).
            remote_safe: true,
            hint: "在開啟的網址完成授權後，把驗證碼貼回下方並按 Enter。",
        }),
        RuntimeType::Codex => Some(CliAuthSpec {
            runtime,
            login_args: v(&["login"]),
            success_markers: v(&["successfully", "logged in", "authenticated"]),
            failure_markers: v(&["login failed", "authentication failed", "invalid", "access denied"]),
            // codex login uses a localhost callback.
            remote_safe: false,
            hint: "於同機瀏覽器完成 OpenAI 登入（localhost 回呼）。遠端請改用 API key。",
        }),
        RuntimeType::Gemini => Some(CliAuthSpec {
            runtime,
            login_args: v(&["auth", "login"]),
            success_markers: v(&["successfully", "logged in", "authenticated", "credentials saved"]),
            failure_markers: v(&["login failed", "authentication failed", "invalid"]),
            remote_safe: false,
            hint: "於同機瀏覽器完成 Google 登入（localhost 回呼）。",
        }),
        RuntimeType::Antigravity => Some(CliAuthSpec {
            runtime,
            login_args: v(&["login"]),
            success_markers: v(&["successfully", "authenticated", "auth-success", "signed in"]),
            failure_markers: v(&["login failed", "authentication failed", "invalid"]),
            // agy uses an oauth-callback (antigravity.google/oauth-callback).
            remote_safe: false,
            hint: "於同機瀏覽器完成 Antigravity 登入。遠端請改用 ANTIGRAVITY_API_KEY。",
        }),
        RuntimeType::OpenAiCompat => None,
    }
}

/// Resolve the login program path for a runtime (`None` ⇒ CLI not installed).
pub fn resolve_program(runtime: RuntimeType) -> Option<String> {
    match runtime {
        RuntimeType::Claude => duduclaw_core::which_claude(),
        RuntimeType::Codex => duduclaw_core::which_codex(),
        RuntimeType::Gemini => duduclaw_core::which_gemini(),
        RuntimeType::Antigravity => duduclaw_core::which_agy(),
        RuntimeType::OpenAiCompat => None,
    }
}

/// Login session status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStatus {
    Running,
    Succeeded,
    Failed,
    /// Process exited without a clear success/failure marker.
    Exited,
}

const ST_RUNNING: u8 = 0;
const ST_SUCCEEDED: u8 = 1;
const ST_FAILED: u8 = 2;
const ST_EXITED: u8 = 3;

impl AuthStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            ST_SUCCEEDED => Self::Succeeded,
            ST_FAILED => Self::Failed,
            ST_EXITED => Self::Exited,
            _ => Self::Running,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Exited => "exited",
        }
    }
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Pure marker scanner over a (lowercased) rolling output tail. Failure wins
/// over success when both are present in the same window. `None` ⇒ keep running.
pub fn scan_outcome(tail_lower: &str, spec: &CliAuthSpec) -> Option<AuthStatus> {
    if spec.failure_markers.iter().any(|m| tail_lower.contains(m.as_str())) {
        return Some(AuthStatus::Failed);
    }
    if spec.success_markers.iter().any(|m| tail_lower.contains(m.as_str())) {
        return Some(AuthStatus::Succeeded);
    }
    None
}

/// Errors starting a login session.
#[derive(Debug)]
pub enum AuthError {
    /// Runtime has no interactive login (OpenAI-compat).
    NoLogin,
    /// CLI binary not found on PATH / known locations.
    NotInstalled,
    Pty(String),
    Spawn(String),
    Io(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLogin => write!(f, "this runtime has no interactive login (use an API key)"),
            Self::NotInstalled => write!(f, "CLI binary not found"),
            Self::Pty(e) => write!(f, "pty error: {e}"),
            Self::Spawn(e) => write!(f, "spawn error: {e}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}
impl std::error::Error for AuthError {}

/// A live interactive login session driving a CLI's native login command in a PTY.
pub struct AuthSession {
    pub id: String,
    pub runtime: RuntimeType,
    pub spec: CliAuthSpec,
    pub program: String,
    status: Arc<AtomicU8>,
    output_tx: broadcast::Sender<Vec<u8>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    // Master is retained so the PTY stays open for the session's lifetime.
    _master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
}

impl AuthSession {
    /// Spawn the login command for `runtime` in a PTY. `env` is merged into the
    /// child environment (e.g. proxy vars). Output streams to subscribers.
    pub fn spawn(
        id: String,
        runtime: RuntimeType,
        env: HashMap<String, String>,
    ) -> Result<Arc<Self>, AuthError> {
        let spec = spec_for(runtime).ok_or(AuthError::NoLogin)?;
        let program = resolve_program(runtime).ok_or(AuthError::NotInstalled)?;

        let pty_system = native_pty_system();
        // Wide terminal on purpose: the CLIs render with an Ink TUI that hard-wraps
        // long strings at the column width. An OAuth authorize URL (~350-420 chars
        // with code_challenge/state) wrapped across rows can't be reliably
        // reassembled for the "open this link" button in the dashboard. 600 cols
        // keeps the URL on a single line so the frontend can extract it cleanly.
        let pair = pty_system
            .openpty(PtySize { rows: 40, cols: 600, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| AuthError::Pty(e.to_string()))?;

        let mut cmd = CommandBuilder::new(&program);
        for a in &spec.login_args {
            cmd.arg(a);
        }
        for (k, val) in &env {
            cmd.env(k, val);
        }
        // Surface the auth URL in-band instead of trying to launch a browser the
        // user can't see (esp. headless / remote). `BROWSER=echo` is honoured by
        // most CLIs' "open in browser" helpers.
        cmd.env("BROWSER", "echo");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AuthError::Spawn(e.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AuthError::Pty(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| AuthError::Pty(e.to_string()))?;

        let (output_tx, _rx) = broadcast::channel::<Vec<u8>>(1024);
        let status = Arc::new(AtomicU8::new(ST_RUNNING));

        // Reader thread: blocking PTY read → broadcast bytes + scan a rolling
        // lowercased tail for success/failure markers.
        {
            let tx = output_tx.clone();
            let status = status.clone();
            let spec = spec.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut tail = String::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            let _ = tx.send(chunk.to_vec());
                            tail.push_str(&String::from_utf8_lossy(chunk).to_lowercase());
                            if tail.len() > 8192 {
                                let cut = tail.len() - 4096;
                                tail.drain(..cut);
                            }
                            if status.load(Ordering::Relaxed) == ST_RUNNING {
                                if let Some(o) = scan_outcome(&tail, &spec) {
                                    let code = match o {
                                        AuthStatus::Succeeded => ST_SUCCEEDED,
                                        AuthStatus::Failed => ST_FAILED,
                                        _ => ST_RUNNING,
                                    };
                                    status.store(code, Ordering::Relaxed);
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                // Process closed the PTY. If no marker was seen, mark Exited.
                if status.load(Ordering::Relaxed) == ST_RUNNING {
                    status.store(ST_EXITED, Ordering::Relaxed);
                }
            });
        }

        Ok(Arc::new(Self {
            id,
            runtime,
            spec,
            program,
            status,
            output_tx,
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            _master: Mutex::new(pair.master),
        }))
    }

    /// Subscribe to the raw output byte stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    /// Write user input to the login process (e.g. a pasted code + `\r`).
    pub fn write_input(&self, data: &[u8]) -> Result<(), AuthError> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)
            .and_then(|_| w.flush())
            .map_err(|e| AuthError::Io(e.to_string()))
    }

    /// Current status.
    pub fn status(&self) -> AuthStatus {
        AuthStatus::from_u8(self.status.load(Ordering::Relaxed))
    }

    /// Kill the login process (cancel).
    pub fn kill(&self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cli_has_a_login_spec_except_openai_compat() {
        for rt in [
            RuntimeType::Claude,
            RuntimeType::Codex,
            RuntimeType::Gemini,
            RuntimeType::Antigravity,
        ] {
            let spec = spec_for(rt).unwrap_or_else(|| panic!("{rt:?} must have a login spec"));
            assert!(!spec.login_args.is_empty(), "{rt:?} login_args empty");
            assert!(!spec.success_markers.is_empty(), "{rt:?} no success markers");
            assert!(!spec.hint.is_empty());
        }
        assert!(spec_for(RuntimeType::OpenAiCompat).is_none());
    }

    #[test]
    fn claude_is_remote_safe_others_are_not() {
        // setup-token is paste-back → remote ok; the rest use localhost callbacks.
        assert!(spec_for(RuntimeType::Claude).unwrap().remote_safe);
        assert!(!spec_for(RuntimeType::Codex).unwrap().remote_safe);
        assert!(!spec_for(RuntimeType::Gemini).unwrap().remote_safe);
        assert!(!spec_for(RuntimeType::Antigravity).unwrap().remote_safe);
    }

    #[test]
    fn scan_outcome_detects_success_and_failure() {
        let spec = spec_for(RuntimeType::Claude).unwrap();
        assert_eq!(scan_outcome("…you can now use claude", &spec), Some(AuthStatus::Succeeded));
        assert_eq!(scan_outcome("error: invalid code", &spec), Some(AuthStatus::Failed));
        assert_eq!(scan_outcome("visit https://… to authorize", &spec), None);
    }

    #[test]
    fn scan_outcome_failure_wins_over_success() {
        let spec = spec_for(RuntimeType::Claude).unwrap();
        // both present → failure
        assert_eq!(
            scan_outcome("authenticated but then token expired", &spec),
            Some(AuthStatus::Failed)
        );
    }

    #[test]
    fn openai_compat_has_no_program() {
        assert!(resolve_program(RuntimeType::OpenAiCompat).is_none());
    }

    #[test]
    fn status_terminality_and_strings() {
        assert!(!AuthStatus::Running.is_terminal());
        assert!(AuthStatus::Succeeded.is_terminal());
        assert!(AuthStatus::Failed.is_terminal());
        assert!(AuthStatus::Exited.is_terminal());
        assert_eq!(AuthStatus::Succeeded.as_str(), "succeeded");
    }

    /// LIVE: drives the *real* `claude setup-token` through the AuthSession PTY
    /// driver and captures its streamed output (the auth URL), then kills the
    /// session WITHOUT completing the login (no account is bound). Proves the
    /// driver launches the real CLI and streams it. Env-gated + ignored.
    #[tokio::test]
    #[ignore = "live: runs real `claude setup-token`; needs claude installed"]
    async fn live_claude_login_streams_output() {
        let session = AuthSession::spawn("live-test".to_string(), RuntimeType::Claude, HashMap::new())
            .expect("spawn claude setup-token");
        let program = session.program.clone();
        let mut rx = session.subscribe();
        let mut captured = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(bytes)) => {
                    captured.push_str(&String::from_utf8_lossy(&bytes));
                    // Stop only on an actual auth link / explicit paste prompt.
                    let low = captured.to_lowercase();
                    if low.contains("http://") || low.contains("https://") || low.contains("authorize") {
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => {}
            }
        }
        session.kill();
        // Strip ANSI/cursor escapes for a readable transcript.
        let clean: String = {
            let mut out = String::new();
            let mut chars = captured.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    // skip CSI/escape sequence until a letter terminator
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if n.is_ascii_alphabetic() || n == '~' { break; }
                    }
                } else if c == '\u{7}' || c == '\r' {
                    // drop bell / carriage return
                } else {
                    out.push(c);
                }
            }
            out
        };
        eprintln!(
            "\n=== LIVE claude login: program={program} status={:?} raw={} bytes ===\n{}\n=== end ===",
            session.status(),
            captured.len(),
            clean.trim()
        );
        assert!(
            !captured.is_empty(),
            "expected streamed output from real `claude setup-token` (got nothing)"
        );
    }
}
