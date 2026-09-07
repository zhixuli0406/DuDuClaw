// Interactive CLI login, driven from the OOBE `RuntimeAuth` step — WP-C of
// `docs/todo/TODO-ai-runtimes-2026-09.md` (2026-09-05).
//
// UNVERIFIED: needs live gateway. Every wire shape below is copied from
// `duduclaw-gateway/src/handlers.rs` on `main` as of 2026-09-05
// (`handle_cli_login_start` / `_input` / `_status` / `_cancel` /
// `_finalize`, plus the `auth.cli_login.output` / `auth.cli_login.status`
// events its forwarder task emits) and cross-checked against the dashboard
// component that already drives them (`web/src/components/CliLoginModal.tsx`
// + `web/src/lib/cli-auth-url.ts`) — but nothing here has been exercised
// against a running gateway from this crate. The parsing half IS covered by
// unit tests against real-shaped transcripts; the RPC half is not, and the
// TODO's own §4 sequencing says WP-C's live check waits on WP-A/WP-B landing.
//
// ── The RPCs this module depends on ──────────────────────────────────────
//   auth.cli_login.start  {runtime}            -> {session_id, runtime,
//                                                  program, remote_safe,
//                                                  hint, status}
//   auth.cli_login.status {session_id}         -> {session_id, status}
//   auth.cli_login.cancel {session_id}         -> {success}
//   auth.cli_login.finalize {session_id}       -> {registered, reason?,
//                                                  account_id?, store?}
//   event auth.cli_login.output {session_id, data}   (raw PTY bytes)
//   event auth.cli_login.status {session_id, status} (terminal only)
//
// `status` is one of `running` / `succeeded` / `failed` / `exited`
// (`cli_auth::AuthStatus::as_str`). Anything else is treated as "still
// running" here, never as success — an unrecognised status must not read as
// an authorized machine.
//
// ── Why the transcript is parsed at all ──────────────────────────────────
// The CLIs render a full-screen Ink TUI; the verification URL and (for the
// device-code flows) the user code exist only inside that redraw stream. The
// dashboard solves this with `stripAnsi` + `extractAuthUrl`; both are ported
// here rather than shared, because the shell has no JavaScript and no regex
// dependency (see `Cargo.toml`) — the ranking RULES are the shared thing,
// and `cli-auth-url.ts`'s own header comment is the specification they came
// from.
//
// ── Threading ────────────────────────────────────────────────────────────
// Same contract as every other module in this tree: everything here BLOCKS
// and is run from a `std::thread::spawn`, reporting back over an
// `mpsc::Sender`. Nothing here touches gpui.

use std::cell::RefCell;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde_json::json;

use super::ws_rpc::{self, RpcError, StreamStep};

/// How long the streaming connection stays open waiting for the CLI's own
/// output and its terminal status event. A device-code login is a
/// human-paced flow (open a browser, sign in, approve), so this is generous
/// — but bounded, because a walked-away-from login must not pin a thread and
/// a socket for the life of the process.
const STREAM_BUDGET: Duration = Duration::from_secs(150);

/// After the stream closes without a terminal status, the worker falls back
/// to polling `auth.cli_login.status`. This is the TOTAL wall-clock ceiling
/// for one login attempt, streaming included.
const OVERALL_BUDGET: Duration = Duration::from_secs(300);

/// Gap between `auth.cli_login.status` polls in that fallback.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How much of the PTY transcript is kept while scanning for a URL / code.
/// The dashboard keeps 20 000 characters; the same ceiling here bounds
/// memory on a redraw-heavy TUI that can emit megabytes.
const MAX_TRANSCRIPT: usize = 20_000;

/// What the worker thread reports back to the OOBE step. One enum rather
/// than a struct-with-options so an impossible combination (a code before a
/// session exists, a success with no session) cannot be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CliLoginUpdate {
    /// The gateway accepted the request and spawned the CLI.
    Started { session_id: String },
    /// The transcript now yields a URL and/or a device code. Sent whenever
    /// either changes, so the screen can fill in as the CLI prints.
    Prompt { url: Option<String>, code: Option<String> },
    /// A terminal status arrived. `registered` is `auth.cli_login.finalize`'s
    /// own answer for a success (`false` is NOT a failure — most CLIs persist
    /// their own credential store and have no token to register; see
    /// `handle_cli_login_finalize`'s `cli_store` branch).
    Settled { status: CliLoginStatus, registered: bool },
    /// The login never got off the ground, or the connection failed before a
    /// terminal status. `kind` is what the screen branches on; `detail` is
    /// for the log only — the step renders its own localized copy and never
    /// shows this text.
    Failed { kind: CliLoginFailure, detail: String },
}

/// Why a login attempt never reached a terminal status. Two kinds, both
/// classified from a TYPED [`RpcError`] variant rather than by sniffing an
/// error string — the same discipline this crate applies everywhere a
/// decision hangs on an error (see `RpcError::RateLimited`'s own doc
/// comment): a `contains("not installed")` check would silently stop
/// matching the moment the gateway rewords its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliLoginFailure {
    /// The gateway itself refused the request (`RpcError::Rejected`): that
    /// CLI is not installed on this machine, or the runtime has no
    /// interactive login at all. Retrying changes nothing.
    Rejected,
    /// Could not reach the local service, the session never settled inside
    /// its budget, or the response was unusable. Worth another try.
    Unreachable,
}

impl CliLoginFailure {
    fn from_rpc(e: &RpcError) -> Self {
        match e {
            RpcError::Rejected(_) => CliLoginFailure::Rejected,
            _ => CliLoginFailure::Unreachable,
        }
    }
}

/// The three terminal states `cli_auth::AuthStatus` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliLoginStatus {
    Succeeded,
    /// The CLI reported an authentication failure.
    Failed,
    /// The CLI exited without ever reporting either — a cancel, a crash, or
    /// a login the operator abandoned in the browser.
    Exited,
}

impl CliLoginStatus {
    /// `None` for `running` and for anything unrecognised — see this file's
    /// header comment: an unknown status must never read as success.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "succeeded" => Some(CliLoginStatus::Succeeded),
            "failed" => Some(CliLoginStatus::Failed),
            "exited" => Some(CliLoginStatus::Exited),
            _ => None,
        }
    }
}

/// Runs one login attempt start to finish. Blocking; the caller owns the
/// thread. Every outcome — including "could not even start" — is reported
/// through `tx`, so the UI never has to infer anything from silence.
pub(crate) fn run_login_session(jwt: String, runtime: &'static str, tx: Sender<CliLoginUpdate>) {
    let started_at = Instant::now();
    // Shared between the two closures below — the response handler WRITES
    // it, the event handler READS it to filter the broadcast. Two closures
    // handed to the same call cannot borrow one local mutably and immutably,
    // so the sharing is explicit. `RefCell`, not a lock: both closures run
    // on this one thread, one at a time, inside `call_then_stream_events`.
    let session_id: RefCell<Option<String>> = RefCell::new(None);
    let mut transcript = String::new();
    let mut last_url: Option<String> = None;
    let mut last_code: Option<String> = None;
    let mut terminal: Option<CliLoginStatus> = None;

    let stream = ws_rpc::call_then_stream_events(
        &jwt,
        "auth.cli_login.start",
        json!({ "runtime": runtime }),
        STREAM_BUDGET,
        |payload| {
            match payload.get("session_id").and_then(|v| v.as_str()) {
                Some(sid) if !sid.is_empty() => {
                    *session_id.borrow_mut() = Some(sid.to_string());
                    let _ = tx.send(CliLoginUpdate::Started { session_id: sid.to_string() });
                    StreamStep::Continue
                }
                // A response with no session id is unusable — there is
                // nothing to poll, cancel or finalize.
                _ => StreamStep::Stop,
            }
        },
        |event, payload| {
            // Every socket sees every broadcast event, so a concurrent login
            // started elsewhere (the dashboard, another OOBE run) would show
            // up here too. Filter on OUR session id, always.
            let ours = session_id.borrow();
            let Some(ours) = ours.as_deref() else { return StreamStep::Continue };
            if payload.get("session_id").and_then(|v| v.as_str()) != Some(ours) {
                return StreamStep::Continue;
            }
            match event {
                "auth.cli_login.output" => {
                    let data = payload.get("data").and_then(|v| v.as_str()).unwrap_or("");
                    push_transcript(&mut transcript, data);
                    let clean = strip_ansi(&transcript);
                    let url = extract_auth_url(&clean);
                    let code = extract_device_code(&clean, url.as_deref());
                    if url != last_url || code != last_code {
                        last_url = url.clone();
                        last_code = code.clone();
                        let _ = tx.send(CliLoginUpdate::Prompt { url, code });
                    }
                    StreamStep::Continue
                }
                "auth.cli_login.status" => match payload.get("status").and_then(|v| v.as_str()).and_then(CliLoginStatus::parse) {
                    Some(status) => {
                        terminal = Some(status);
                        StreamStep::Stop
                    }
                    None => StreamStep::Continue,
                },
                _ => StreamStep::Continue,
            }
        },
    );

    if let Err(e) = stream {
        // `session_id` being set means the request WAS accepted and only the
        // stream died — the poll fallback below can still settle it.
        if session_id.borrow().is_none() {
            let _ = tx.send(CliLoginUpdate::Failed { kind: CliLoginFailure::from_rpc(&e), detail: format!("{e:?}") });
            return;
        }
        eprintln!("[oobe/cli-login] transcript stream ended early ({e:?}) — falling back to status polling");
    }

    let Some(sid) = session_id.into_inner() else {
        let _ = tx.send(CliLoginUpdate::Failed {
            kind: CliLoginFailure::Unreachable,
            detail: "the gateway returned no session id".to_string(),
        });
        return;
    };

    // Fallback: the stream can end before the CLI settles (budget spent, the
    // gateway's own 60s no-pong reaper, a dropped connection). `auth.cli_
    // login.status` is authoritative either way.
    while terminal.is_none() && started_at.elapsed() < OVERALL_BUDGET {
        std::thread::sleep(POLL_INTERVAL);
        match status_of(&jwt, &sid) {
            Ok(Some(status)) => terminal = Some(status),
            Ok(None) => {}
            Err(e) => {
                let _ = tx.send(CliLoginUpdate::Failed { kind: CliLoginFailure::from_rpc(&e), detail: format!("{e:?}") });
                return;
            }
        }
    }

    let Some(status) = terminal else {
        // Out of budget with the CLI still running. Kill the session rather
        // than leaving an orphaned PTY on the appliance.
        let _ = cancel(&jwt, &sid);
        let _ = tx.send(CliLoginUpdate::Failed {
            kind: CliLoginFailure::Unreachable,
            detail: "login did not complete inside its budget".to_string(),
        });
        return;
    };

    let registered = if status == CliLoginStatus::Succeeded {
        match finalize(&jwt, &sid) {
            Ok(registered) => registered,
            Err(e) => {
                // The login itself succeeded; only the account registration
                // round trip failed. Report the success honestly and say the
                // registration did not happen.
                eprintln!("[oobe/cli-login] auth.cli_login.finalize failed after a successful login: {e:?}");
                false
            }
        }
    } else {
        false
    };
    let _ = tx.send(CliLoginUpdate::Settled { status, registered });
}

/// `auth.cli_login.status` — `None` while the session is still running (or
/// reports something this build does not recognise).
pub(crate) fn status_of(jwt: &str, session_id: &str) -> Result<Option<CliLoginStatus>, RpcError> {
    let payload = ws_rpc::call_once(jwt, "auth.cli_login.status", json!({ "session_id": session_id }))?;
    Ok(payload.get("status").and_then(|v| v.as_str()).and_then(CliLoginStatus::parse))
}

/// `auth.cli_login.cancel` — kills the PTY. Best effort by nature: a session
/// the gateway has already forgotten answers "login session not found",
/// which is the same end state the caller wanted.
pub(crate) fn cancel(jwt: &str, session_id: &str) -> Result<(), RpcError> {
    ws_rpc::call_once(jwt, "auth.cli_login.cancel", json!({ "session_id": session_id }))?;
    Ok(())
}

/// `auth.cli_login.finalize` — turns a scraped long-lived token into an
/// `[[accounts]]` row. Answers `registered: false` for every CLI that keeps
/// its own credential store (`reason: "cli_store"`), which is a SUCCESS
/// path, not a failure — see `handle_cli_login_finalize`'s own comment.
pub(crate) fn finalize(jwt: &str, session_id: &str) -> Result<bool, RpcError> {
    let payload = ws_rpc::call_once(jwt, "auth.cli_login.finalize", json!({ "session_id": session_id }))?;
    Ok(payload.get("registered").and_then(|v| v.as_bool()).unwrap_or(false))
}

/// Appends to the rolling transcript, keeping only the newest
/// [`MAX_TRANSCRIPT`] characters. Trims on a CHARACTER boundary (never a
/// byte one) — PTY output is UTF-8 and these CLIs print CJK.
fn push_transcript(transcript: &mut String, data: &str) {
    transcript.push_str(data);
    let len = transcript.chars().count();
    if len > MAX_TRANSCRIPT {
        let skip = len - MAX_TRANSCRIPT;
        *transcript = transcript.chars().skip(skip).collect();
    }
}

/// Strips ANSI / VT escape sequences so the transcript is scannable text.
/// A port of `web/src/lib/cli-auth-url.ts`'s companion `stripAnsi` in
/// `CliLoginModal.tsx`, written as a state machine rather than five regexes
/// (this crate has no regex dependency — see `Cargo.toml`).
///
/// Not a terminal emulator: it does not replay cursor movement, so a TUI
/// redraw still leaves interleaved fragments. It only has to leave URLs and
/// codes intact, which it does because the gateway gives the CLI a
/// 600-column PTY (`cli_auth.rs`) precisely so a long authorize URL stays on
/// one line.
pub(crate) fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            // Drop the control characters a TUI sprays around: BEL,
            // backspace, vertical tab, form feed. Newline/tab/CR stay —
            // they are what keeps the transcript line-shaped.
            if matches!(c, '\u{7}' | '\u{8}' | '\u{b}' | '\u{c}') {
                continue;
            }
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: ESC [ <params> <final byte in @A-Za-z>
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '@' {
                        break;
                    }
                }
            }
            // OSC: ESC ] … terminated by BEL or ST (ESC \)
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Charset selection: ESC ( X / ESC ) X / ESC # X / ESC % X
            Some('(') | Some(')') | Some('#') | Some('%') => {
                chars.next();
            }
            // Every other single-character escape (ESC =, ESC >, ESC 7, …).
            Some(_) => {}
            None => break,
        }
    }
    out
}

/// OAuth parameters that only ever appear on a page a human must open.
const OAUTH_PARAMS: [&str; 8] = ["client_id=", "response_type=", "redirect_uri=", "code_challenge=", "user_code=", "device_code=", "scope=", "state="];

/// Path fragments a CLI prints but nobody should be sent to: callback
/// endpoints, post-login landing pages, reference links. Removed BEFORE
/// ranking, exactly as `cli-auth-url.ts` documents — a docs link with a
/// `?utm_source=` query or a callback echo carrying `?code=&state=` would
/// otherwise out-rank the real destination.
const NOT_A_DESTINATION: [&str; 11] =
    ["oauth-callback", "oauthcallback", "auth-success", "authsuccess", "/docs", "/doc", "/terms", "/privacy", "/support", "/changelog", "/help"];

/// Shape match kept as a fallback for transcripts with no query strings.
const LOOKS_LIKE_AUTH: [&str; 4] = ["oauth", "authorize", "auth.", "/cai/"];

/// Trailing characters a TUI may append to a URL (box borders, prose).
const TRAILING_JUNK: [char; 10] = ['.', ',', ';', ':', ')', ']', '}', '>', '\'', '"'];

/// Pulls the sign-in URL out of an ANSI-stripped transcript.
///
/// The ranking rules are `web/src/lib/cli-auth-url.ts`'s, verbatim in
/// intent: known non-destinations are dropped first, then a URL carrying
/// OAuth parameters wins (the LAST one — a TUI redraws, and after a retry
/// the newest URL is the live one), then any other parameterised URL, then a
/// shape match, then the longest remaining candidate.
pub(crate) fn extract_auth_url(clean: &str) -> Option<String> {
    let urls: Vec<String> = find_urls(clean);
    if urls.is_empty() {
        return None;
    }
    let plausible: Vec<&String> = urls.iter().filter(|u| !is_not_a_destination(u)).collect();
    let pool: Vec<&String> = if plausible.is_empty() { urls.iter().collect() } else { plausible };

    let with_query: Vec<&&String> = pool.iter().filter(|u| u.contains('?')).collect();
    if let Some(u) = with_query.iter().rev().find(|u| {
        let lower = u.to_ascii_lowercase();
        OAUTH_PARAMS.iter().any(|p| lower.contains(&format!("?{p}")) || lower.contains(&format!("&{p}")))
    }) {
        return Some((**u).clone());
    }
    if let Some(u) = with_query.last() {
        return Some((**u).clone());
    }
    if let Some(u) = pool.iter().find(|u| {
        let lower = u.to_ascii_lowercase();
        LOOKS_LIKE_AUTH.iter().any(|m| lower.contains(m))
    }) {
        return Some((*u).clone());
    }
    pool.into_iter().max_by_key(|u| u.len()).cloned()
}

fn is_not_a_destination(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    NOT_A_DESTINATION.iter().any(|m| lower.contains(m))
}

/// Every `http(s)://…` run in the text, trimmed of trailing punctuation.
/// Stops a URL at whitespace and at the quote/bracket characters a TUI draws
/// boxes with — the same terminator set `cli-auth-url.ts`'s own regex uses.
fn find_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest: String = bytes[i..].iter().take(8).collect();
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let mut j = i;
            while j < bytes.len() && !matches!(bytes[j], ' ' | '\t' | '\n' | '\r' | '"' | '\'' | '<' | '>' | ')' | ']') {
                j += 1;
            }
            let candidate: String = bytes[i..j].iter().collect();
            let trimmed = candidate.trim_end_matches(TRAILING_JUNK);
            // "http://" with no authority points nowhere.
            if trimmed.len() > "https://".len() {
                out.push(trimmed.to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// The device code the operator has to type into the verification page, or
/// `None` when this flow has none (every browser-callback login).
///
/// Two sources, strongest first:
///   1. `user_code=` inside the authorize URL — device-authorization
///      endpoints carry it there, and it is unambiguous.
///   2. A bare `XXXX-XXXX` token in the transcript, the shape the
///      device-code CLIs print for a human to read.
///
/// The bare-token scan requires at least one ASCII UPPERCASE LETTER in the
/// code, so a date (`2026-09-05`) or a plain numeric pair can never be
/// mistaken for one. The LAST match wins, same "a TUI redraws, newest is
/// live" reasoning [`extract_auth_url`] uses.
pub(crate) fn extract_device_code(clean: &str, url: Option<&str>) -> Option<String> {
    if let Some(url) = url {
        if let Some(code) = user_code_param(url) {
            return Some(code);
        }
    }
    let mut found: Option<String> = None;
    for token in clean.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
        if looks_like_device_code(token) {
            found = Some(token.to_string());
        }
    }
    found
}

fn user_code_param(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("user_code=") {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn looks_like_device_code(token: &str) -> bool {
    let Some((left, right)) = token.split_once('-') else { return false };
    if right.contains('-') {
        return false;
    }
    let group_ok = |g: &str| g.len() == 4 && g.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    group_ok(left) && group_ok(right) && token.chars().any(|c| c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parses_only_the_three_terminal_states() {
        assert_eq!(CliLoginStatus::parse("succeeded"), Some(CliLoginStatus::Succeeded));
        assert_eq!(CliLoginStatus::parse("failed"), Some(CliLoginStatus::Failed));
        assert_eq!(CliLoginStatus::parse("exited"), Some(CliLoginStatus::Exited));
        assert_eq!(CliLoginStatus::parse("running"), None);
        // The load-bearing one: an unrecognised status must never read as
        // success (see this module's header comment).
        assert_eq!(CliLoginStatus::parse("Succeeded"), None);
        assert_eq!(CliLoginStatus::parse(""), None);
        assert_eq!(CliLoginStatus::parse("ok"), None);
    }

    #[test]
    fn strip_ansi_removes_csi_osc_and_control_bytes_but_keeps_the_text() {
        let raw = "\u{1b}[2J\u{1b}[1;32mOpen \u{1b}[0mhttps://example.com/auth\u{7}\n\u{1b}]0;title\u{7}done";
        assert_eq!(strip_ansi(raw), "Open https://example.com/auth\ndone");
    }

    #[test]
    fn strip_ansi_keeps_cjk_and_newlines() {
        assert_eq!(strip_ansi("\u{1b}[33m請在瀏覽器完成登入\u{1b}[0m\n代碼：ABCD-1234"), "請在瀏覽器完成登入\n代碼：ABCD-1234");
    }

    #[test]
    fn strip_ansi_survives_a_truncated_escape_at_the_end() {
        assert_eq!(strip_ansi("text\u{1b}"), "text");
        assert_eq!(strip_ansi("text\u{1b}[1;3"), "text");
    }

    #[test]
    fn the_oauth_parameterised_url_wins_over_a_callback_and_a_docs_link() {
        // The `agy` shape `cli-auth-url.ts` was written against: a callback
        // endpoint and a docs link printed alongside the real consent URL.
        let clean = "Visit https://antigravity.google/oauth-callback and read https://example.com/docs?utm_source=cli\n\
                     Open this: https://accounts.google.com/o/oauth2/auth?client_id=123&redirect_uri=http%3A%2F%2Flocalhost";
        assert_eq!(
            extract_auth_url(clean).as_deref(),
            Some("https://accounts.google.com/o/oauth2/auth?client_id=123&redirect_uri=http%3A%2F%2Flocalhost")
        );
    }

    #[test]
    fn a_localhost_listener_line_never_beats_the_consent_url() {
        let clean = "listening on http://localhost:1455\nGo to https://auth.openai.com/authorize?response_type=code&state=xyz";
        assert_eq!(extract_auth_url(clean).as_deref(), Some("https://auth.openai.com/authorize?response_type=code&state=xyz"));
    }

    #[test]
    fn the_newest_oauth_url_wins_after_a_retry_redraw() {
        let clean = "https://x.ai/device?user_code=OLD1-2AAA\nretrying…\nhttps://x.ai/device?user_code=NEW9-8BBB";
        assert_eq!(extract_auth_url(clean).as_deref(), Some("https://x.ai/device?user_code=NEW9-8BBB"));
    }

    #[test]
    fn trailing_punctuation_and_box_borders_are_trimmed_off_the_url() {
        let clean = "│ open https://example.com/auth?client_id=1. │";
        assert_eq!(extract_auth_url(clean).as_deref(), Some("https://example.com/auth?client_id=1"));
    }

    #[test]
    fn a_transcript_with_no_url_yields_nothing_rather_than_a_guess() {
        assert_eq!(extract_auth_url("Waiting for the browser…"), None);
        assert_eq!(extract_auth_url(""), None);
    }

    #[test]
    fn a_shape_match_is_the_fallback_when_no_url_carries_a_query() {
        assert_eq!(extract_auth_url("go to https://github.com/login/oauth now").as_deref(), Some("https://github.com/login/oauth"));
    }

    #[test]
    fn the_device_code_comes_from_the_url_when_it_carries_one() {
        let url = "https://github.com/login/device?user_code=ABCD-1234";
        assert_eq!(extract_device_code("open the link", Some(url)).as_deref(), Some("ABCD-1234"));
    }

    #[test]
    fn a_bare_device_code_in_the_transcript_is_found() {
        let clean = "First copy your one-time code: WXYZ-9876\nThen press Enter to open github.com";
        assert_eq!(extract_device_code(clean, None).as_deref(), Some("WXYZ-9876"));
    }

    #[test]
    fn a_date_is_never_mistaken_for_a_device_code() {
        // Requires an uppercase letter precisely so `2026-0905`-shaped text
        // and numeric pairs cannot masquerade as a code.
        assert_eq!(extract_device_code("build 2026-0905 ready", None), None);
        assert_eq!(extract_device_code("pair 1234-5678", None), None);
    }

    #[test]
    fn a_browser_callback_login_reports_no_device_code() {
        let clean = "Opening https://auth.openai.com/authorize?response_type=code in your browser";
        let url = extract_auth_url(clean);
        assert_eq!(extract_device_code(clean, url.as_deref()), None);
    }

    #[test]
    fn the_transcript_buffer_is_bounded_and_keeps_the_newest_text() {
        let mut transcript = String::new();
        for _ in 0..40 {
            push_transcript(&mut transcript, &"x".repeat(1000));
        }
        push_transcript(&mut transcript, "CODE-HERE");
        assert_eq!(transcript.chars().count(), MAX_TRANSCRIPT);
        assert!(transcript.ends_with("CODE-HERE"), "the newest output must survive the trim");
    }

    #[test]
    fn the_transcript_trim_never_splits_a_multibyte_character() {
        let mut transcript = String::new();
        for _ in 0..30 {
            push_transcript(&mut transcript, &"請在瀏覽器完成登入".repeat(100));
        }
        // Reaching here at all means no byte-boundary panic; assert the
        // content is still valid, whole characters.
        assert_eq!(transcript.chars().count(), MAX_TRANSCRIPT);
        assert!(transcript.contains('請'));
    }
}
