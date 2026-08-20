//! WP-D: "訂閱帳號" device-code-style setup wizard for headless boxes.
//!
//! A headless box (no browser reachable at `localhost:<port>`) cannot complete
//! `claude login`'s interactive OAuth — the redirect targets the CLI's own
//! machine, not the operator's browser. `claude setup-token` is the CLI's
//! paste-back alternative: it prints an authorize URL, waits for a pasted
//! code, then prints a long-lived `CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-…`
//! exactly once (never persisted to disk by the CLI itself). This module
//! drives that flow end to end for a guided 3-step dashboard wizard:
//!
//!   1. `accounts.setup_token_start`  — spawn `claude setup-token` in a PTY,
//!      return `{session_id, auth_url}` (the URL is usually ready within a
//!      few seconds; the caller can also poll `status`).
//!   2. `accounts.setup_token_submit` — paste the code back, wait
//!      synchronously for the CLI to finish, capture the token, VALIDATE it
//!      with a real API round-trip, and only then persist it via the
//!      existing `accounts.add` path.
//!   3. `accounts.setup_token_status` / `accounts.setup_token_cancel`.
//!
//! Every low-level primitive (PTY driving, ANSI stripping, token/URL
//! extraction, marker scanning) is reused from [`crate::cli_auth`] — the
//! generic multi-runtime "Dashboard 一鍵登入" (`auth.cli_login.*`) already
//! implements those correctly and this module does not re-derive them. What
//! this module adds is the orchestration the generic flow does not need for
//! its power-user, stream-and-watch UX: synchronous submit-and-wait, pre-store
//! validation, one global concurrent session, a TTL, and a small closed set of
//! machine-readable error codes a friendlier wizard UI can key copy off.
//!
//! ## Why `claude auth status` is NOT the validator (real finding, 2026-08-18)
//!
//! `duduclaw_agent::account_rotator::probe_and_restore` treats `claude auth
//! status` as an OAuth health probe — but that is only safe for an
//! ALREADY-TRUSTED account cooling down from a rate limit. For a FRESHLY
//! CAPTURED, never-verified token it is unsafe: a live check on a real
//! machine —
//!
//! ```text
//! $ env -i PATH="$PATH" HOME="$HOME" \
//!     CLAUDE_CODE_OAUTH_TOKEN="sk-ant-oat01-totallyFAKEvalue0000000000000000000000000000" \
//!     claude auth status
//! {"loggedIn":true,"authMethod":"oauth_token","apiProvider":"firstParty"}
//! ```
//!
//! returned `loggedIn: true` for an obviously invalid token — `claude auth
//! status` only checks that the env var is non-empty; it never calls the
//! network. The only real validator is an actual API round-trip:
//!
//! ```text
//! $ env -i PATH="$PATH" HOME="$HOME" \
//!     CLAUDE_CODE_OAUTH_TOKEN="sk-ant-oat01-totallyFAKEvalue0000000000000000000000000000" \
//!     claude -p "reply with the single word: pong" --output-format json
//! {"is_error":true,...,"api_error_status":401,
//!  "result":"Failed to authenticate. API Error: 401 OAuth access token is invalid."}
//! ```
//!
//! correctly failed for the same fake token (and a genuine token round-trips
//! `is_error:false` — confirmed live with `--model haiku` from an empty scratch
//! `cwd`, ~22K baseline cache-creation tokens from the CLI's own tool schemas,
//! no CLAUDE.md auto-discovery cost). [`validate_captured_token`] is that call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{info, warn};

use crate::cli_auth::{AuthError, AuthSession, AuthStatus};
use duduclaw_core::types::RuntimeType;

/// Session TTL for the whole wizard flow (start → submit), per WP-D spec.
/// Enforced lazily (every RPC touching the slot checks `created_at.elapsed()`)
/// AND by a background sweep spawned in `start` (kills the PTY child even if
/// the dashboard never calls back in — an abandoned browser tab must not leave
/// a `claude setup-token` process running forever).
pub const SETUP_TOKEN_TTL: Duration = Duration::from_secs(300);

/// Bounded wait in `start` for the auth URL to appear before responding, so
/// the RPC stays snappy instead of blocking on however long the CLI takes to
/// print its first screen. Real captures show the URL within ~4s; the caller
/// can also poll `status` if it isn't ready yet.
const AUTH_URL_WAIT: Duration = Duration::from_secs(8);
const AUTH_URL_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Bounded wait in `submit` for the CLI to reach a terminal status after the
/// code is written. `claude setup-token` exchanges the code for a token in a
/// single network round-trip — this is generous headroom, not the expected
/// case.
const SUBMIT_WAIT: Duration = Duration::from_secs(60);
const SUBMIT_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Timeout for the post-capture validation call (`claude -p`).
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(30);

/// Structured error codes the wizard's RPCs can return in
/// `WsFrame` `error.code`. Stable strings — the dashboard keys its zh-TW copy
/// off these, never off a raw (potentially secret-bearing) CLI transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupTokenErrorCode {
    /// `claude` binary not resolvable on this host.
    NotInstalled,
    /// `session_id` doesn't match the current (or any) active slot.
    NoActiveSession,
    /// The session's TTL elapsed before `submit` completed.
    Expired,
    /// The CLI rejected the pasted code (or exited without ever confirming
    /// it — same user-facing remedy: paste the code again).
    InvalidCode,
    /// `submit` waited past `SUBMIT_WAIT` with no terminal status.
    Timeout,
    /// A token WAS captured but the post-capture API round-trip failed —
    /// never persisted.
    ValidationFailed,
    /// A second `submit` arrived while the first was still in flight.
    AlreadySubmitting,
    /// PTY / process spawn failure unrelated to the code itself.
    Io,
}

impl SetupTokenErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotInstalled => "not_installed",
            Self::NoActiveSession => "no_active_session",
            Self::Expired => "expired",
            Self::InvalidCode => "invalid_code",
            Self::Timeout => "timeout",
            Self::ValidationFailed => "validation_failed",
            Self::AlreadySubmitting => "already_submitting",
            Self::Io => "io_error",
        }
    }
}

/// One active wizard session's bookkeeping. Wraps the generic
/// [`crate::cli_auth::AuthSession`] (which does the actual PTY work) with the
/// single-slot/TTL/in-flight-submit state this wizard's synchronous contract
/// needs. `Clone` is cheap (Arc fields) — handlers clone the slot out of the
/// server's `RwLock` and drop the lock before doing any long `.await`.
#[derive(Clone)]
pub struct SetupTokenSlot {
    pub session_id: String,
    pub session: Arc<AuthSession>,
    pub created_at: Instant,
    /// CAS guard so two concurrent `submit` calls on the same session can't
    /// both drive the PTY writer at once.
    submitting: Arc<AtomicBool>,
}

impl SetupTokenSlot {
    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > SETUP_TOKEN_TTL
    }

    pub fn expires_in_seconds(&self) -> u64 {
        SETUP_TOKEN_TTL
            .saturating_sub(self.created_at.elapsed())
            .as_secs()
    }

    /// Atomically claim the "submit in flight" flag. `false` ⇒ a concurrent
    /// `submit` already claimed it — the caller must refuse this one with
    /// [`SetupTokenErrorCode::AlreadySubmitting`] rather than write a second,
    /// interleaved code into the same PTY. Never released: every submit
    /// outcome (success or failure) ends with the slot being discarded, so
    /// a fresh `start` is required to try again — matching how the CLI's own
    /// terminal `Failed`/`Exited` status already works for the generic
    /// `cli_login.*` flow this reuses.
    pub fn try_begin_submit(&self) -> bool {
        !self.submitting.swap(true, Ordering::SeqCst)
    }
}

/// Spawn `claude setup-token` in a PTY (via [`AuthSession`]) and arm the
/// background TTL sweep. Returns the fresh slot; the caller is responsible
/// for installing it as the server's single active session (replacing /
/// killing any prior one first — that single-concurrency policy lives in the
/// RPC handler, not here, since it needs the server's lock).
pub fn spawn_session(session_id: String) -> Result<SetupTokenSlot, AuthError> {
    let session = AuthSession::spawn(session_id.clone(), RuntimeType::Claude, std::collections::HashMap::new())?;

    // TTL sweep: guarantees the PTY child is reaped even if the dashboard
    // never calls back in (closed tab, network drop). Idempotent — killing an
    // already-terminal session is a harmless no-op in `AuthSession::kill`.
    {
        let sess = session.clone();
        let sid = session_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SETUP_TOKEN_TTL).await;
            if !sess.status().is_terminal() {
                info!(
                    target: "setup_token_wizard",
                    session = %sid,
                    "TTL sweep: killing abandoned setup-token session"
                );
                sess.kill();
            }
        });
    }

    Ok(SetupTokenSlot {
        session_id,
        session,
        created_at: Instant::now(),
        submitting: Arc::new(AtomicBool::new(false)),
    })
}

/// Bounded poll for the auth URL to appear in the session's captured output
/// (see [`crate::cli_auth::AuthSession::captured_auth_url`]). Returns early on
/// success OR once the session reaches a terminal status (nothing more will
/// ever be captured). Never errors — `None` just means "not ready yet, poll
/// `status`".
pub async fn wait_for_auth_url(session: &AuthSession) -> Option<String> {
    let deadline = Instant::now() + AUTH_URL_WAIT;
    loop {
        if let Some(url) = session.captured_auth_url() {
            return Some(url);
        }
        if session.status().is_terminal() {
            return session.captured_auth_url();
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(AUTH_URL_POLL_INTERVAL).await;
    }
}

/// Outcome of driving a pasted code through to completion.
pub enum SubmitOutcome {
    /// Login succeeded and a token was captured and validated.
    Validated(String),
    /// Login succeeded but no token was ever captured (should not happen for
    /// the Claude runtime — `claude setup-token` always prints one on
    /// success — surfaced as `ValidationFailed` by the caller).
    NoToken,
    /// A token was captured but failed live validation. Message is a
    /// sanitized, non-secret-bearing summary — never the raw CLI transcript.
    ValidationFailed(String),
    /// The CLI's own markers reported failure, or the process exited without
    /// ever confirming success (garbled/expired code, Enter never landed).
    InvalidCode,
    /// No terminal status within `SUBMIT_WAIT`.
    Timeout,
}

/// Write the pasted code into the session's PTY, wait synchronously for a
/// terminal status, and — on success — validate the captured token with a
/// real API call. Does NOT touch the caller's session-slot bookkeeping; the
/// RPC handler owns clearing the slot once this returns.
///
/// The write itself splits the code from its trailing Enter into two
/// separate PTY writes with a short pause between them — the same fix
/// `handle_cli_login_input` uses for the identical Ink masked-input quirk in
/// `claude setup-token`'s "paste code here" prompt (a code + Enter arriving
/// in the SAME write is swallowed as one paste and never submits).
pub async fn submit_code(claude_bin: &str, session: &AuthSession, code: &str) -> SubmitOutcome {
    let code = code.trim();
    if let Err(e) = session.write_input(code.as_bytes()) {
        warn!(target: "setup_token_wizard", error = %e, "failed to write code to PTY");
        return SubmitOutcome::Timeout;
    }
    tokio::time::sleep(Duration::from_millis(350)).await;
    if let Err(e) = session.write_input(b"\r") {
        warn!(target: "setup_token_wizard", error = %e, "failed to write Enter to PTY");
        return SubmitOutcome::Timeout;
    }

    let deadline = Instant::now() + SUBMIT_WAIT;
    loop {
        let status = session.status();
        if status.is_terminal() {
            match status {
                AuthStatus::Succeeded => break,
                // Failed (CLI's own "invalid code" marker) or Exited (process
                // ended with no marker at all — in practice also a rejected
                // code) both map to the same user-facing remedy: try again.
                AuthStatus::Failed | AuthStatus::Exited => return SubmitOutcome::InvalidCode,
                AuthStatus::Running => unreachable!("is_terminal() false-positive"),
            }
        }
        if Instant::now() >= deadline {
            return SubmitOutcome::Timeout;
        }
        tokio::time::sleep(SUBMIT_POLL_INTERVAL).await;
    }

    let Some(token) = session.captured_token() else {
        return SubmitOutcome::NoToken;
    };

    match validate_captured_token(claude_bin, &token).await {
        Ok(()) => SubmitOutcome::Validated(token),
        Err(reason) => SubmitOutcome::ValidationFailed(reason),
    }
}

/// Validate a freshly captured OAuth token with a REAL API round-trip. See
/// the module doc for why `claude auth status` cannot be used here. Runs
/// `claude -p "<tiny prompt>" --output-format json --model haiku` with an
/// allowlisted, cleared environment (per the P3 credentials-doctrine spawn-env
/// scrub — `duduclaw_core::spawn_env`) carrying ONLY the captured token as
/// Anthropic auth, from an empty scratch `cwd` (no CLAUDE.md auto-discovery,
/// minimal baseline token cost). Returns `Ok(())` on `is_error: false`; `Err`
/// carries a sanitized (non-secret) summary — never the raw token, never the
/// raw subprocess output verbatim.
///
/// ## `USER` must NOT reach this spawn (real finding, 2026-08-18)
///
/// The base allowlist includes `USER` (needed elsewhere — shell tooling,
/// general CLI operation) but on a machine that ALSO has a real OS-keychain
/// `claude` session (any dev box, and plausibly an operator's desktop), its
/// presence changes `claude -p`'s behavior for an invalid
/// `CLAUDE_CODE_OAUTH_TOKEN`: bisected live —
///
/// ```text
/// PATH+HOME+CLAUDE_CODE_OAUTH_TOKEN=<fake>            → is_error:true,  api_error_status:401  (correct)
/// PATH+HOME+USER+CLAUDE_CODE_OAUTH_TOKEN=<fake>       → is_error:false                          (WRONG — silently fell back to keychain, validated nothing)
/// ```
/// i.e. an obviously-fake token was reported valid — using the machine's OWN
/// keychain session, not the token being tested — the moment `USER` was in
/// the child's env. `LOGNAME` was bisected separately and does NOT trigger
/// it, but is stripped too out of caution (same "which OS user" signal).
/// Every other allowlisted var (`LOGNAME`/`SHELL`/`LANG`/`TERM`/`TMPDIR`/`TZ`/
/// `__CF_USER_TEXT_ENCODING`) was confirmed safe together. Without this,
/// `validate_captured_token` would rubber-stamp ANY string as a valid
/// subscription token on a machine where the operator (or, in dev, this
/// crate's own test runner) already has an independent keychain login —
/// exactly the class of bug this function exists to prevent (see the
/// module-level `claude auth status` finding above).
pub async fn validate_captured_token(claude_bin: &str, token: &str) -> Result<(), String> {
    let scratch_dir = std::env::temp_dir().join("duduclaw-setup-token-validate");
    if let Err(e) = tokio::fs::create_dir_all(&scratch_dir).await {
        return Err(format!("could not prepare a scratch directory: {e}"));
    }

    let mut cmd = duduclaw_core::platform::async_command_for(claude_bin);
    duduclaw_core::spawn_env::apply_agent_cli_env_allowlist(&mut cmd);
    cmd.env("CLAUDE_CODE_OAUTH_TOKEN", token);
    cmd.env_remove("CLAUDECODE");
    // See the doc comment above — closes a real keychain-fallback gap.
    cmd.env_remove("USER");
    cmd.env_remove("LOGNAME");
    cmd.current_dir(&scratch_dir);
    cmd.args([
        "-p",
        "reply with the single word: OK",
        "--output-format",
        "json",
        "--model",
        "haiku",
    ]);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("could not start the verification call: {e}")),
    };

    let output = match tokio::time::timeout(VALIDATE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("verification call I/O error: {e}")),
        Err(_) => return Err("verification call timed out".to_string()),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Parse ONLY the specific fields we need; never forward raw stdout/stderr
    // (defense in depth — a future CLI version echoing the prompt or env back
    // must not leak anything through this error path).
    let parsed: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    let is_error = parsed
        .as_ref()
        .and_then(|v| v.get("is_error"))
        .and_then(Value::as_bool);

    match is_error {
        Some(false) => Ok(()),
        Some(true) => {
            let api_status = parsed
                .as_ref()
                .and_then(|v| v.get("api_error_status"))
                .and_then(Value::as_i64);
            Err(match api_status {
                Some(401) => "帳號驗證失敗（401）：剛取得的授權碼可能已失效。".to_string(),
                Some(code) => format!("驗證呼叫回傳錯誤（API {code}）。"),
                None => "驗證呼叫回傳失敗，但沒有明確的錯誤碼。".to_string(),
            })
        }
        None => Err(format!(
            "無法解析驗證呼叫的回應（exit={:?}）。",
            output.status.code()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_code_has_a_stable_non_empty_string() {
        let all = [
            SetupTokenErrorCode::NotInstalled,
            SetupTokenErrorCode::NoActiveSession,
            SetupTokenErrorCode::Expired,
            SetupTokenErrorCode::InvalidCode,
            SetupTokenErrorCode::Timeout,
            SetupTokenErrorCode::ValidationFailed,
            SetupTokenErrorCode::AlreadySubmitting,
            SetupTokenErrorCode::Io,
        ];
        let mut seen = std::collections::HashSet::new();
        for code in all {
            let s = code.as_str();
            assert!(!s.is_empty());
            assert!(s.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{s} not snake_case");
            assert!(seen.insert(s), "duplicate error code string: {s}");
        }
    }

    /// LIVE: proves `spawn_session` + `wait_for_auth_url` work end to end
    /// against the REAL `claude setup-token` (same non-destructive pattern as
    /// `cli_auth::tests::live_claude_login_streams_output` — the session is
    /// killed before any code is ever pasted, so no account is bound and
    /// nothing about this machine's real login is touched). Env-gated +
    /// ignored, matching this crate's existing convention for tests that
    /// need a real `claude` binary.
    #[tokio::test]
    #[ignore = "live: runs real `claude setup-token`; needs claude installed"]
    async fn live_spawn_session_and_wait_for_auth_url() {
        let slot = spawn_session("live-wizard-test".to_string()).expect("spawn setup-token session");
        assert!(!slot.is_expired());
        assert!(slot.expires_in_seconds() > 0);

        let url = wait_for_auth_url(&slot.session).await;
        slot.session.kill();

        let url = url.expect("expected an auth_url to be captured within the wait window");
        assert!(url.starts_with("https://"), "url = {url}");
        assert!(url.contains("claude.com"), "url = {url}");
        assert!(url.contains("code_challenge="), "expected a PKCE-shaped consent URL: {url}");
    }

    /// LIVE: proves `validate_captured_token` performs a REAL API round-trip
    /// (not the `claude auth status` false-positive documented in the module
    /// doc) by feeding it a token that is syntactically token-shaped but
    /// definitely not a valid credential, and asserting it comes back `Err`
    /// classified from a real 401. No account/credential on this machine is
    /// touched — the call only ever sends the fake token. Env-gated +
    /// ignored, same convention as the other live tests in this crate.
    #[tokio::test]
    #[ignore = "live: spawns real `claude -p`; needs claude installed"]
    async fn live_validate_captured_token_rejects_a_fake_token() {
        let Some(claude_bin) = crate::cli_auth::resolve_program(RuntimeType::Claude) else {
            panic!("claude CLI not found — required for this live test");
        };
        let fake_token = "sk-ant-oat01-totallyFAKEvalue0000000000000000000000000000";
        let result = validate_captured_token(&claude_bin, fake_token).await;
        let err = result.expect_err("a fake token must fail live validation");
        assert!(
            err.contains("401") || err.contains("驗證"),
            "expected a 401-classified or generic validation-failure message, got: {err}"
        );
        // The fake token itself must never appear in the error message.
        assert!(!err.contains(fake_token), "error message must not echo the token: {err}");
    }
}
