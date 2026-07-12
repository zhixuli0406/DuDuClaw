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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

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
    /// Path (relative to `$HOME`) of the credentials file the CLI writes on a
    /// successful login. When set, a watcher thread marks the session
    /// `Succeeded` the moment this file is created/updated — a deterministic
    /// signal that does NOT depend on scraping the TUI's success wording (which
    /// changes across CLI versions and can be silent). `None` ⇒ marker-only.
    pub success_file: Option<&'static str>,
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Resolve `$HOME/<rel>` for the success-file watcher.
fn home_join(rel: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rel))
}

fn file_mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Strip ANSI/VT escape sequences, preserving every non-escape character (case
/// included). Correctly consumes:
///   - CSI: ESC `[` (params 0x30-0x3F)* (intermediates 0x20-0x2F)* final 0x40-0x7E
///   - OSC: ESC `]` … (BEL, or ST = `ESC \`)
///   - other 2-char escapes: ESC <byte>
///
/// A previous version stopped a CSI at "the first ASCII letter", which OVER-RUNS
/// when the final byte is not a letter (cursor ops ending in `@`, `` ` ``, etc.):
/// it then ate the following character. That silently dropped a char from scraped
/// OAuth tokens (`sk-ant-oat01-…` → `sk-ant-at01-…`), yielding an invalid token
/// and a 401 when the agent tried to reply.
fn strip_ansi_keep_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next(); // consume '['
                    // params/intermediates, then exactly one final byte 0x40-0x7E
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next(); // consume ']'
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next(); // 2-char escape: ESC <byte>
                }
                None => {}
            }
        } else if c != '\u{7}' && c != '\u{8}' {
            out.push(c);
        }
    }
    out
}

/// Extract a long-lived auth token (`sk-ant-…`) from CLI output. `claude
/// setup-token` PRINTS the token once (`export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-…`)
/// and never persists it — so the one-click-login flow must scrape it here to
/// register an account. The PTY is wide (600 cols) so the token stays on one
/// line; ANSI is stripped first, then we take the contiguous token-char run.
pub fn extract_oauth_token(raw: &str) -> Option<String> {
    let clean = strip_ansi_keep_case(raw);
    let idx = clean.find("sk-ant-")?;
    let tok: String = clean[idx..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (tok.len() >= 24).then_some(tok)
}

/// Redact token-like runs (e.g. `sk-ant-oat01-…`) from normalized PTY text
/// before it is logged. The success screen may echo the long-lived token.
fn redact_for_log(s: &str) -> String {
    let mut out = s.to_string();
    while let Some(i) = out.find("sk-ant") {
        let tail = &out[i + 6..];
        let n = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').count();
        out.replace_range(i..i + 6 + n, "<redacted-token>");
    }
    out
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
            // Claude Code writes the OAuth token here on success (Linux headless).
            // Watching it is the reliable success signal — the TUI's success text
            // is escape-laden and version-dependent, and may not print at all.
            success_file: Some(".claude/.credentials.json"),
        }),
        RuntimeType::Codex => Some(CliAuthSpec {
            runtime,
            login_args: v(&["login"]),
            success_markers: v(&["successfully", "logged in", "authenticated"]),
            failure_markers: v(&["login failed", "authentication failed", "invalid", "access denied"]),
            // codex login uses a localhost callback.
            remote_safe: false,
            hint: "於同機瀏覽器完成 OpenAI 登入（localhost 回呼）。遠端請改用 API key。",
            success_file: Some(".codex/auth.json"),
        }),
        RuntimeType::Gemini => Some(CliAuthSpec {
            runtime,
            login_args: v(&["auth", "login"]),
            success_markers: v(&["successfully", "logged in", "authenticated", "credentials saved"]),
            failure_markers: v(&["login failed", "authentication failed", "invalid"]),
            remote_safe: false,
            hint: "於同機瀏覽器完成 Google 登入（localhost 回呼）。",
            success_file: Some(".gemini/oauth_creds.json"),
        }),
        RuntimeType::Antigravity => Some(CliAuthSpec {
            runtime,
            login_args: v(&["login"]),
            success_markers: v(&["successfully", "authenticated", "auth-success", "signed in"]),
            failure_markers: v(&["login failed", "authentication failed", "invalid"]),
            // agy uses an oauth-callback (antigravity.google/oauth-callback).
            remote_safe: false,
            hint: "於同機瀏覽器完成 Antigravity 登入。遠端請改用 ANTIGRAVITY_API_KEY。",
            success_file: None,
        }),
        // R4 phase 1: Grok CLI detection + headless spawn only. The SuperGrok
        // OAuth device-flow (accounts.x.ai) is a phase-2 follow-up, and its exact
        // `grok` login subcommand/markers are UNVERIFIED, so no interactive login
        // spec is wired yet (fail-closed: `None` means "no verified login flow").
        // The binary is still discoverable via `resolve_program` below.
        RuntimeType::Grok => None,
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
        RuntimeType::Grok => duduclaw_core::which_grok(),
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

/// Normalize CLI output for marker matching: strip ANSI/VT escape sequences,
/// lowercase, and drop ALL whitespace. The login CLIs render with a full-screen
/// Ink TUI that positions each word with cursor-control escapes rather than
/// literal spaces — so in the raw stream "Invalid code" arrives as
/// `Invalid<ESC>[…code`, and a multi-word marker like "invalid code" never
/// appears as contiguous text. Normalizing both sides (here + the markers) makes
/// substring matching robust against the TUI's redraw. Without this, every
/// multi-word marker silently fails and the dashboard spins on "進行中" forever.
fn normalize_for_match(s: &str) -> String {
    strip_ansi_keep_case(s)
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Pure marker scanner over a rolling raw output tail (ANSI included). Both the
/// tail and the markers are normalized (ANSI-stripped, whitespace-removed) before
/// matching — see [`normalize_for_match`]. Failure wins over success when both
/// are present in the same window. `None` ⇒ keep running.
pub fn scan_outcome(tail: &str, spec: &CliAuthSpec) -> Option<AuthStatus> {
    let hay = normalize_for_match(tail);
    let norm = |m: &str| -> String {
        m.chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect()
    };
    if spec.failure_markers.iter().any(|m| hay.contains(&norm(m))) {
        return Some(AuthStatus::Failed);
    }
    if spec.success_markers.iter().any(|m| hay.contains(&norm(m))) {
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
    // Long-lived OAuth token scraped from the output (`claude setup-token` prints
    // it once and never persists it). Used to register an account on success.
    token: Arc<Mutex<Option<String>>>,
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
        let token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        // Success-file watcher: the most reliable signal that login succeeded is
        // the CLI writing its credentials file — independent of the TUI's success
        // wording (escape-laden, version-dependent, sometimes silent). Polls for
        // the file to appear/advance past its baseline mtime, up to ~10 min.
        if let Some(path) = spec.success_file.and_then(home_join) {
            let status_w = status.clone();
            let baseline = file_mtime(&path);
            let log_id = id.clone();
            std::thread::spawn(move || {
                for _ in 0..600 {
                    std::thread::sleep(Duration::from_secs(1));
                    if status_w.load(Ordering::Relaxed) != ST_RUNNING {
                        return;
                    }
                    if let Some(m) = file_mtime(&path) {
                        let advanced = baseline.map(|b| m > b).unwrap_or(true);
                        if advanced {
                            status_w.store(ST_SUCCEEDED, Ordering::Relaxed);
                            tracing::info!(target: "cli_auth", session = %log_id, file = %path.display(), "login success: credentials file written");
                            return;
                        }
                    }
                }
            });
        }

        // Reader thread: blocking PTY read → broadcast bytes + scan a rolling RAW
        // tail (scan_outcome normalizes ANSI/whitespace before matching markers).
        {
            let tx = output_tx.clone();
            let status = status.clone();
            let spec = spec.clone();
            let log_id = id.clone();
            let token_store = token.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let mut tail = String::new();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            let _ = tx.send(chunk.to_vec());
                            tail.push_str(&String::from_utf8_lossy(chunk));
                            if tail.len() > 16384 {
                                let cut = tail.len() - 8192;
                                tail.drain(..cut);
                            }
                            // Scrape the long-lived OAuth token (printed once by
                            // `setup-token`). Re-extract each chunk so a token that
                            // arrives split across reads converges to the full value.
                            if let Some(t) = extract_oauth_token(&tail) {
                                *token_store.lock().unwrap() = Some(t);
                            }
                            // Diagnostic: log a redacted, normalized snapshot of the
                            // tail end so the live transcript is inspectable from the
                            // gateway log when a login gets stuck.
                            let snap = normalize_for_match(&String::from_utf8_lossy(chunk));
                            let snap: String = snap.chars().rev().take(160).collect::<Vec<_>>().into_iter().rev().collect();
                            tracing::info!(target: "cli_auth", session = %log_id, bytes = n, snap = %redact_for_log(&snap), "pty output");
                            if status.load(Ordering::Relaxed) == ST_RUNNING {
                                if let Some(o) = scan_outcome(&tail, &spec) {
                                    let code = match o {
                                        AuthStatus::Succeeded => ST_SUCCEEDED,
                                        AuthStatus::Failed => ST_FAILED,
                                        _ => ST_RUNNING,
                                    };
                                    status.store(code, Ordering::Relaxed);
                                    tracing::info!(target: "cli_auth", session = %log_id, outcome = ?o, "marker matched");
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
                tracing::info!(target: "cli_auth", session = %log_id, final_status = ?AuthStatus::from_u8(status.load(Ordering::Relaxed)), "pty reader exited");
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
            token,
        }))
    }

    /// The long-lived OAuth token scraped from the login output, if any.
    pub fn captured_token(&self) -> Option<String> {
        self.token.lock().unwrap().clone()
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
    fn scan_outcome_matches_through_ink_tui_escapes() {
        // The real failure case: Ink positions each word with cursor escapes, so
        // "Invalid code" arrives with escapes between the words. The scanner must
        // still flag it (regression: pre-normalization this matched nothing and
        // the dashboard span on "進行中" forever).
        let spec = spec_for(RuntimeType::Claude).unwrap();
        let tui_invalid = "OAuth error: \u{1b}[31mInvalid\u{1b}[2G\u{1b}[Kcode\u{1b}[0m. Press Enter to try.";
        assert_eq!(scan_outcome(tui_invalid, &spec), Some(AuthStatus::Failed));
        let tui_ok = "Login \u{1b}[32msuccess\u{1b}[0mful! Token \u{1b}[1msaved\u{1b}[0m.";
        assert_eq!(scan_outcome(tui_ok, &spec), Some(AuthStatus::Succeeded));
        // The authorize URL/prompt must NOT trip either outcome.
        let prompt = "Browser didn't open? \u{1b}[2GUse the url below\n\u{1b}[3Ghttps://claude.com/cai/oauth/authorize?code=true&scope=user:inference\nPaste code here if prompted >";
        assert_eq!(scan_outcome(prompt, &spec), None);
    }

    #[test]
    fn extract_oauth_token_from_setup_token_output() {
        // `claude setup-token` success line, with TUI escapes sprinkled in.
        let out = "Success! \u{1b}[1mStore this token securely.\u{1b}[0m\nexport CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-AbC123_def-456GHI\nYou won't be able to see it again.";
        assert_eq!(
            extract_oauth_token(out).as_deref(),
            Some("sk-ant-oat01-AbC123_def-456GHI")
        );
        // No token in the prompt/URL phase.
        assert_eq!(extract_oauth_token("Paste code here >").as_deref(), None);
        // Too-short junk isn't mistaken for a token.
        assert_eq!(extract_oauth_token("sk-ant-x").as_deref(), None);
    }

    #[test]
    fn extract_oauth_token_survives_non_letter_csi_final() {
        // Regression: a CSI ending in a NON-letter final byte (here `@` = 0x40)
        // right before the token must not eat the next char. The old "stop at the
        // first ASCII letter" stripper dropped the 'o' (`oat01` → `at01`),
        // producing an invalid token → 401 on reply.
        let out = "export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-\u{1b}[1@oat01-AbC_def-123XyZ456ghiJKLmno";
        assert_eq!(
            extract_oauth_token(out).as_deref(),
            Some("sk-ant-oat01-AbC_def-123XyZ456ghiJKLmno")
        );
        // Cursor-position CSI (final 'G') mid-token also must not drop a char.
        let out2 = "sk-ant-oat\u{1b}[5G01-ZZZZZZZZZZZZZZZZZZZZZZ";
        assert_eq!(
            extract_oauth_token(out2).as_deref(),
            Some("sk-ant-oat01-ZZZZZZZZZZZZZZZZZZZZZZ")
        );
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
