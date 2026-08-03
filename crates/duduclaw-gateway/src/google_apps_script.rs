//! Apps Script bridge — the third Google credential source, and the only one
//! that works for personal `@gmail.com` accounts.
//!
//! **Why**: the OAuth path needs a Google-verified app before it may serve
//! outside our own domain; domain-wide delegation (see
//! [`crate::google_service_account`]) needs a Workspace domain and a super
//! admin. A customer with a personal Gmail account and no IT department can use
//! neither. This path moves the authorization into *their* account: they deploy
//! `templates/apps-script/duduclaw-bridge.gs` as a web app under their own
//! login, and DuDuClaw calls that URL. There is no third-party app to verify
//! and no client id to create — Google only ever sees the user running their
//! own script.
//!
//! **The URL plus the secret are a credential.** Together they grant whatever
//! the deployed script can do (mail read + draft, calendar, sheets). They are
//! stored the same way channel bot tokens are — encrypted at rest via
//! [`crate::config_crypto`] — and the request host is allow-listed so a
//! mistyped or tampered `url` cannot ship the secret to an attacker's server.
//!
//! **Coverage is a subset.** The bridge implements eight actions (Gmail search
//! / read / draft, Calendar list / create, Sheets read / append, status). Drive,
//! Docs, Slides, Forms and Tasks stay OAuth/service-account only; asking for
//! them on this path returns an explicit "not available on the bridge" error
//! rather than an empty result.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// Hosts the bridge may talk to. A deployed Apps Script web app lives at
/// `script.google.com` and 302-redirects its response through
/// `script.googleusercontent.com`; nothing else is ever a legitimate hop.
///
/// This is a security gate, so it is exact host equality — never a substring
/// check (project convention #2: `script.google.com.evil.test` must not pass).
const ALLOWED_HOSTS: &[&str] = &["script.google.com", "script.googleusercontent.com"];

/// The bridge is a user-deployed script running Gmail/Calendar queries; a few
/// seconds is normal, a minute means something is wrong.
const HTTP_TIMEOUT_SECS: u64 = 60;

/// Actions the shipped `duduclaw-bridge.gs` implements. Kept as a typed list so
/// a caller cannot invent an action name the script will reject at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeAction {
    Status,
    GmailSearch,
    GmailRead,
    GmailCreateDraft,
    CalendarListEvents,
    CalendarCreateEvent,
    SheetsRead,
    SheetsAppend,
}

impl BridgeAction {
    /// Wire name — must match the `switch` in `duduclaw-bridge.gs`.
    pub fn as_str(self) -> &'static str {
        match self {
            BridgeAction::Status => "status",
            BridgeAction::GmailSearch => "gmail_search",
            BridgeAction::GmailRead => "gmail_read",
            BridgeAction::GmailCreateDraft => "gmail_create_draft",
            BridgeAction::CalendarListEvents => "calendar_list_events",
            BridgeAction::CalendarCreateEvent => "calendar_create_event",
            BridgeAction::SheetsRead => "sheets_read",
            BridgeAction::SheetsAppend => "sheets_append",
        }
    }
}

/// Operator configuration read from `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    /// The deployed web-app `/exec` URL.
    pub url: String,
    /// Shared secret, already decrypted. Never logged, never echoed.
    pub secret: String,
}

impl BridgeConfig {
    /// Redacted debug view for logs — proves configuration without leaking it.
    pub fn describe(&self) -> String {
        format!("apps-script bridge at {} (secret set)", host_of(&self.url).unwrap_or_default())
    }
}

/// Failures using the bridge. Never carries the secret.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("`[integrations.google_apps_script] url` must be an https script.google.com /exec URL")]
    InvalidUrl,
    #[error("`[integrations.google_apps_script] secret` is missing or empty")]
    MissingSecret,
    #[error("Apps Script bridge request failed: {0}")]
    RequestFailed(String),
    #[error("Apps Script bridge returned a non-JSON response (is the deployment set to \"Anyone\" access?)")]
    MalformedResponse,
    /// The script itself reported a problem — `unauthorized` means the secret in
    /// `config.toml` and the `SECRET` in the deployed script disagree.
    #[error("Apps Script bridge error: {0}")]
    Script(String),
    #[error(
        "`{0}` is not available through the Apps Script bridge (it covers Gmail, Calendar and Sheets). Connect Google via OAuth or a service account to use Drive/Docs/Slides/Forms/Tasks."
    )]
    Unsupported(String),
}

/// Parse `[integrations.google_apps_script]` from an already-read `config.toml`.
///
/// `decrypt` resolves the stored secret, mirroring how channel bot tokens are
/// handled: an encrypted value if the operator saved one, otherwise the literal
/// string. Injected so this stays a pure, testable function.
///
/// Returns `Ok(None)` when the section is absent. Present-but-broken is an
/// error, never a silent `None` — a typo must not quietly disable the bridge.
pub fn parse_config<F>(raw_toml: &str, decrypt: F) -> Result<Option<BridgeConfig>, BridgeError>
where
    F: Fn(&str) -> String,
{
    let Ok(table) = raw_toml.parse::<toml::Table>() else {
        return Ok(None);
    };
    let Some(section) = table
        .get("integrations")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("google_apps_script"))
        .and_then(|v| v.as_table())
    else {
        return Ok(None);
    };

    let url = section
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    validate_url(&url)?;

    let secret = section
        .get("secret")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(BridgeError::MissingSecret)?;
    let secret = decrypt(secret);
    if secret.trim().is_empty() {
        return Err(BridgeError::MissingSecret);
    }

    Ok(Some(BridgeConfig { url, secret }))
}

/// Read + decrypt the bridge config for a DuDuClaw home. Unreadable config is
/// "not configured" (same fail-safe posture as the other integration gates).
pub fn config_for_home(home_dir: &Path) -> Result<Option<BridgeConfig>, BridgeError> {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return Ok(None);
    };
    parse_config(&raw, |stored| {
        crate::config_crypto::decrypt_value(stored, home_dir).unwrap_or_else(|| stored.to_string())
    })
}

/// Host of a URL, lowercased. `None` when the URL does not parse.
fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(|h| h.to_ascii_lowercase())
}

/// Validate a bridge URL: https, an allow-listed host (exact match), and the
/// Apps Script `/exec` path. Fail-closed — the secret travels in this request,
/// so anything unrecognized is rejected rather than attempted.
pub fn validate_url(url: &str) -> Result<(), BridgeError> {
    let parsed = url::Url::parse(url).map_err(|_| BridgeError::InvalidUrl)?;
    if parsed.scheme() != "https" {
        return Err(BridgeError::InvalidUrl);
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if !ALLOWED_HOSTS.iter().any(|h| *h == host) {
        return Err(BridgeError::InvalidUrl);
    }
    // A "/dev" deployment URL only works for the script owner; "/exec" is the
    // published one. Catching this here saves a confusing 401 later.
    if !parsed.path().ends_with("/exec") {
        return Err(BridgeError::InvalidUrl);
    }
    Ok(())
}

/// The JSON body posted to the script.
#[derive(Debug, Serialize)]
struct BridgeRequest<'a> {
    secret: &'a str,
    action: &'a str,
    params: &'a Value,
}

/// Call one bridge action and return its JSON result.
pub async fn call(
    config: &BridgeConfig,
    action: BridgeAction,
    params: Value,
) -> Result<Value, BridgeError> {
    validate_url(&config.url)?;

    // Redirects are part of the normal Apps Script response flow (302 from
    // script.google.com to script.googleusercontent.com), but each hop is
    // re-checked against the allow-list so a spoofed redirect can never pull
    // the request off Google's infrastructure.
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        let host = attempt.url().host_str().unwrap_or_default().to_ascii_lowercase();
        if attempt.previous().len() > 5 {
            attempt.stop()
        } else if ALLOWED_HOSTS.iter().any(|h| *h == host) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .redirect(policy)
        .build()
        .map_err(|e| BridgeError::RequestFailed(e.to_string()))?;

    let body = BridgeRequest {
        secret: &config.secret,
        action: action.as_str(),
        params: &params,
    };
    let resp = client
        .post(&config.url)
        .json(&body)
        .send()
        .await
        // A transport error can render the request URL; it carries no secret
        // (that lives in the body), but keep it short regardless.
        .map_err(|e| BridgeError::RequestFailed(duduclaw_core::truncate_chars(&e.to_string(), 200)))?;

    let text = resp
        .text()
        .await
        .map_err(|e| BridgeError::RequestFailed(e.to_string()))?;
    parse_response(&text)
}

/// Turn a raw bridge response body into a result. Pure — the whole error
/// surface is testable without a deployed script.
///
/// Apps Script answers a wrong secret with `{"error":"unauthorized"}` and a
/// 200 status, so status alone cannot be trusted; the `error` field is the
/// authority. A non-JSON body means the deployment is misconfigured (an HTML
/// login page is what "Who has access: Only myself" returns).
pub fn parse_response(body: &str) -> Result<Value, BridgeError> {
    let v: Value = serde_json::from_str(body.trim()).map_err(|_| BridgeError::MalformedResponse)?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let hint = if err == "unauthorized" {
            "unauthorized — the secret in config.toml does not match SECRET in the deployed script"
        } else {
            err
        };
        return Err(BridgeError::Script(duduclaw_core::truncate_chars(hint, 300)));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const GOOD_URL: &str = "https://script.google.com/macros/s/AKfycbx123/exec";

    fn plain(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn validate_url_accepts_a_published_exec_url() {
        assert!(validate_url(GOOD_URL).is_ok());
    }

    #[test]
    fn validate_url_rejects_lookalike_hosts() {
        // Exact host equality, not `contains` — the classic allow-list bypass.
        for bad in [
            "https://script.google.com.evil.test/macros/s/x/exec",
            "https://evil.test/script.google.com/exec",
            "https://notscript.google.com/macros/s/x/exec",
        ] {
            assert!(matches!(validate_url(bad), Err(BridgeError::InvalidUrl)), "accepted {bad}");
        }
    }

    #[test]
    fn validate_url_rejects_plaintext_http() {
        // The secret travels in this request — never over http.
        assert!(matches!(
            validate_url("http://script.google.com/macros/s/x/exec"),
            Err(BridgeError::InvalidUrl)
        ));
    }

    #[test]
    fn validate_url_rejects_the_dev_deployment_url() {
        // "/dev" only authorizes the script owner's own browser session; using
        // it would fail confusingly at call time.
        assert!(matches!(
            validate_url("https://script.google.com/macros/s/x/dev"),
            Err(BridgeError::InvalidUrl)
        ));
    }

    #[test]
    fn parse_config_absent_section_is_none() {
        assert_eq!(parse_config("[general]\nlog_level=\"info\"\n", plain).unwrap(), None);
    }

    #[test]
    fn parse_config_reads_url_and_decrypts_secret() {
        let toml = format!(
            "[integrations.google_apps_script]\nurl = \"{GOOD_URL}\"\nsecret = \"ENCRYPTED\"\n"
        );
        let cfg = parse_config(&toml, |s| {
            assert_eq!(s, "ENCRYPTED");
            "plaintext-secret".to_string()
        })
        .unwrap()
        .unwrap();
        assert_eq!(cfg.url, GOOD_URL);
        assert_eq!(cfg.secret, "plaintext-secret");
    }

    #[test]
    fn parse_config_rejects_a_bad_url_instead_of_disabling_silently() {
        let toml = "[integrations.google_apps_script]\nurl = \"https://evil.test/exec\"\nsecret = \"s\"\n";
        assert!(matches!(parse_config(toml, plain), Err(BridgeError::InvalidUrl)));
    }

    #[test]
    fn parse_config_requires_a_secret() {
        let toml = format!("[integrations.google_apps_script]\nurl = \"{GOOD_URL}\"\n");
        assert!(matches!(parse_config(&toml, plain), Err(BridgeError::MissingSecret)));
    }

    #[test]
    fn parse_config_rejects_a_secret_that_decrypts_to_nothing() {
        let toml = format!(
            "[integrations.google_apps_script]\nurl = \"{GOOD_URL}\"\nsecret = \"CORRUPT\"\n"
        );
        assert!(matches!(
            parse_config(&toml, |_| String::new()),
            Err(BridgeError::MissingSecret)
        ));
    }

    #[test]
    fn describe_never_includes_the_secret() {
        let cfg = BridgeConfig { url: GOOD_URL.into(), secret: "hunter2".into() };
        let d = cfg.describe();
        assert!(!d.contains("hunter2"), "leaked secret: {d}");
        assert!(d.contains("script.google.com"), "{d}");
    }

    #[test]
    fn parse_response_returns_the_payload() {
        let v = parse_response(r#"{"messages":[{"subject":"報價"}]}"#).unwrap();
        assert_eq!(v["messages"][0]["subject"], json!("報價"));
    }

    #[test]
    fn parse_response_surfaces_unauthorized_with_an_actionable_hint() {
        // Apps Script answers a wrong secret with HTTP 200 + this body, so the
        // status code alone would read as success.
        let e = parse_response(r#"{"error":"unauthorized"}"#).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("does not match SECRET"), "{msg}");
    }

    #[test]
    fn parse_response_rejects_an_html_login_page() {
        // What a "Who has access: Only myself" deployment returns.
        let e = parse_response("<!DOCTYPE html><html>Sign in</html>").unwrap_err();
        assert!(matches!(e, BridgeError::MalformedResponse), "{e}");
    }

    #[test]
    fn action_wire_names_match_the_deployed_script() {
        // These strings are the contract with duduclaw-bridge.gs's switch.
        assert_eq!(BridgeAction::GmailSearch.as_str(), "gmail_search");
        assert_eq!(BridgeAction::CalendarCreateEvent.as_str(), "calendar_create_event");
        assert_eq!(BridgeAction::SheetsAppend.as_str(), "sheets_append");
        assert_eq!(BridgeAction::Status.as_str(), "status");
    }

    #[test]
    fn unsupported_error_names_the_tool_and_the_alternative() {
        let msg = BridgeError::Unsupported("drive_search".into()).to_string();
        assert!(msg.contains("drive_search"), "{msg}");
        assert!(msg.contains("service account"), "{msg}");
    }
}
