//! Service account + domain-wide delegation — the second Google credential
//! source, for Workspace customers who cannot or will not run an OAuth consent
//! flow per user.
//!
//! **Why**: the OAuth path needs a verified Google app (CASA review for Gmail's
//! restricted scopes) before it can serve customers outside our own domain.
//! Domain-wide delegation sidesteps that entirely: the customer's Workspace
//! super admin authorizes ONE client id in their Admin console
//! (Security → Access and data control → API controls → Manage Domain Wide
//! Delegation) against an explicit scope list, and from then on this process
//! mints tokens for any user in that domain with no consent screen and no app
//! verification.
//!
//! **Limits, stated plainly** — this is not a drop-in replacement for OAuth:
//! - Workspace domains only. Personal `@gmail.com` accounts belong to no
//!   domain and cannot be impersonated; those customers need the OAuth path or
//!   the Apps Script bridge.
//! - It is a high-privilege grant (impersonate anyone in the domain, within the
//!   authorized scopes). Google's own best-practice guidance discourages handing
//!   it to third parties, and since 2024-08 an admin org with multi-party
//!   approval enabled needs a second super admin to sign off.
//!
//! **Precedence**: when a service account is configured it wins over the OAuth
//! vault. Configuring it is a deliberate operator act — silently preferring a
//! stale per-user token would make the operator's intent unobservable.
//!
//! Secret handling: the private key lives in the customer's key file, is read
//! at mint time, and never enters a log line, an error string, or the token
//! cache. Errors carry the failure *shape*, never key material.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::RwLock;

/// Google's OAuth 2.0 token endpoint, used as the JWT `aud` and the POST
/// target. Overridable per key file (`token_uri`) because Google emits it in
/// the JSON and has changed the host before.
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// Assertion lifetime. Google caps this at one hour; we mint short-lived
/// assertions and let the cache handle reuse.
const ASSERTION_TTL_SECS: u64 = 3600;

/// Refresh this long before the token actually expires, so a call that starts
/// just under the wire doesn't race the expiry.
const EXPIRY_SKEW: Duration = Duration::from_secs(120);

/// Bound on the token request. Matches the module's sibling API calls.
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Failures minting a service-account token. Never carries key material.
#[derive(Debug, thiserror::Error)]
pub enum ServiceAccountError {
    #[error("service account key file not found: {0}")]
    KeyFileMissing(String),
    #[error("service account key file is unreadable: {0}")]
    KeyFileUnreadable(String),
    #[error("service account key file is not valid JSON: {0}")]
    KeyFileMalformed(String),
    /// `client_email` / `private_key` absent — usually an OAuth client JSON
    /// pasted in by mistake instead of a service-account key.
    #[error("service account key file is missing `{0}` (is this a service-account key, not an OAuth client?)")]
    KeyFileIncomplete(&'static str),
    #[error("`[integrations.google_service_account] subject` must be the email of a user in the Workspace domain")]
    InvalidSubject,
    #[error("failed to sign the assertion (bad private key?): {0}")]
    SigningFailed(String),
    #[error("token endpoint request failed: {0}")]
    RequestFailed(String),
    /// Google rejected the assertion. The most common cause by far is that the
    /// admin has not authorized this client id for the requested scopes, so the
    /// message says so instead of echoing a bare `unauthorized_client`.
    #[error(
        "Google rejected the service-account assertion ({0}). Check that the Workspace super admin authorized client id {1} for the exact scope list in Admin console → Security → API controls → Manage Domain Wide Delegation."
    )]
    Rejected(String, String),
}

/// The subset of a Google service-account key file we consume.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccountKey {
    pub client_email: String,
    /// PEM-encoded PKCS#8 RSA private key. Never logged, never surfaced.
    pub private_key: String,
    #[serde(default)]
    pub token_uri: Option<String>,
    /// Numeric OAuth client id — the value the customer's admin pastes into the
    /// Admin console. Surfaced in the "not authorized" error so the operator can
    /// copy it straight out of the message.
    #[serde(default)]
    pub client_id: Option<String>,
}

/// Operator configuration read from `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAccountConfig {
    /// Path to the service-account key JSON.
    pub key_file: PathBuf,
    /// The Workspace user to impersonate.
    pub subject: String,
}

/// Parse `[integrations.google_service_account]` out of an already-read
/// `config.toml` body. Pure — unit-testable without a filesystem.
///
/// Returns `Ok(None)` when the section is absent (the overwhelmingly common
/// case: no service account configured). A present-but-incomplete section is an
/// error rather than a silent `None`, so a typo'd key name surfaces instead of
/// quietly falling back to the OAuth path.
pub fn parse_config(
    raw_toml: &str,
    home_dir: &Path,
) -> Result<Option<ServiceAccountConfig>, ServiceAccountError> {
    let Ok(table) = raw_toml.parse::<toml::Table>() else {
        return Ok(None);
    };
    let Some(section) = table
        .get("integrations")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("google_service_account"))
        .and_then(|v| v.as_table())
    else {
        return Ok(None);
    };

    let key_file = section.get("key_file").and_then(|v| v.as_str()).unwrap_or_default();
    let subject = section.get("subject").and_then(|v| v.as_str()).unwrap_or_default();
    build_config(key_file, subject, home_dir).map(Some)
}

/// Validate a `(key_file, subject)` pair and resolve it into a config.
///
/// Shared by [`parse_config`] and the `google.credentials.set` RPC so the
/// dashboard rejects exactly what the config-file path rejects. The RPC used to
/// validate by serializing its pending table and re-parsing it, which silently
/// failed: `toml::Value::to_string()` renders inline-table syntax that is not a
/// TOML document, so every valid save was refused. Validating the values
/// directly removes the round-trip entirely.
pub fn build_config(
    key_file: &str,
    subject: &str,
    home_dir: &Path,
) -> Result<ServiceAccountConfig, ServiceAccountError> {
    let key_file = key_file.trim();
    if key_file.is_empty() {
        return Err(ServiceAccountError::KeyFileIncomplete("key_file"));
    }
    let subject = subject.trim();
    if !is_email_like(subject) {
        return Err(ServiceAccountError::InvalidSubject);
    }

    // A relative key_file resolves against the DuDuClaw home so operators can
    // drop the key next to config.toml without absolute paths.
    let path = Path::new(key_file);
    let key_file = if path.is_absolute() {
        path.to_path_buf()
    } else {
        home_dir.join(path)
    };

    Ok(ServiceAccountConfig {
        key_file,
        subject: subject.to_string(),
    })
}

/// Minimal shape check for an impersonation subject. Not an RFC 5322 validator
/// — it exists to catch "I pasted the project id here", not to police mailbox
/// syntax. The authoritative check is Google rejecting the assertion.
fn is_email_like(s: &str) -> bool {
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// Load and validate a service-account key file.
pub fn load_key(path: &Path) -> Result<ServiceAccountKey, ServiceAccountError> {
    if !path.exists() {
        return Err(ServiceAccountError::KeyFileMissing(path.display().to_string()));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ServiceAccountError::KeyFileUnreadable(e.to_string()))?;
    parse_key(&raw)
}

/// Parse a key file body. Pure — split out so the validation is testable
/// without writing a private key to disk.
pub fn parse_key(raw: &str) -> Result<ServiceAccountKey, ServiceAccountError> {
    let key: ServiceAccountKey = serde_json::from_str(raw).map_err(|e| {
        // The parse error can quote the offending JSON fragment, which for a
        // key file could be part of the private key. Report position only.
        ServiceAccountError::KeyFileMalformed(format!("line {}, column {}", e.line(), e.column()))
    })?;
    if key.client_email.trim().is_empty() {
        return Err(ServiceAccountError::KeyFileIncomplete("client_email"));
    }
    if key.private_key.trim().is_empty() {
        return Err(ServiceAccountError::KeyFileIncomplete("private_key"));
    }
    Ok(key)
}

/// JWT claims for the `urn:ietf:params:oauth:grant-type:jwt-bearer` grant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AssertionClaims {
    /// The service account's own address.
    pub iss: String,
    /// The impersonated Workspace user — the whole point of delegation.
    pub sub: String,
    /// Space-separated scope list. Must be a subset of what the admin
    /// authorized, or Google returns `unauthorized_client`.
    pub scope: String,
    pub aud: String,
    pub iat: u64,
    pub exp: u64,
}

/// Build the assertion claims. Pure — `now_secs` is injected so the expiry
/// arithmetic is testable without a clock.
pub fn build_claims(
    key: &ServiceAccountKey,
    subject: &str,
    scopes: &[&str],
    now_secs: u64,
) -> AssertionClaims {
    AssertionClaims {
        iss: key.client_email.clone(),
        sub: subject.to_string(),
        scope: scopes.join(" "),
        aud: key.token_uri.clone().unwrap_or_else(|| DEFAULT_TOKEN_URI.to_string()),
        iat: now_secs,
        exp: now_secs + ASSERTION_TTL_SECS,
    }
}

/// Cached access token for one `(client_email, subject)` pair.
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    /// When the token stops being usable, already reduced by [`EXPIRY_SKEW`].
    good_until: Instant,
}

/// Process-wide token cache. Keyed by `client_email|subject` so a deployment
/// impersonating several users doesn't cross-contaminate.
fn cache() -> &'static RwLock<HashMap<String, CachedToken>> {
    static CACHE: OnceLock<RwLock<HashMap<String, CachedToken>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Sign the assertion and exchange it for an access token, using the cache when
/// a live token is already on hand.
pub async fn get_token(
    config: &ServiceAccountConfig,
    scopes: &[&str],
) -> Result<String, ServiceAccountError> {
    let key = load_key(&config.key_file)?;
    let cache_key = format!("{}|{}", key.client_email, config.subject);

    if let Some(hit) = cache().read().await.get(&cache_key) {
        if Instant::now() < hit.good_until {
            return Ok(hit.access_token.clone());
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let claims = build_claims(&key, &config.subject, scopes, now);
    let assertion = sign_assertion(&key, &claims)?;

    let token_uri = key.token_uri.clone().unwrap_or_else(|| DEFAULT_TOKEN_URI.to_string());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| ServiceAccountError::RequestFailed(e.to_string()))?;
    let resp = client
        .post(&token_uri)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &assertion),
        ])
        .send()
        .await
        .map_err(|e| ServiceAccountError::RequestFailed(e.to_string()))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let client_id = key.client_id.clone().unwrap_or_else(|| "(see key file `client_id`)".into());
        return Err(ServiceAccountError::Rejected(
            summarize_token_error(&body, status.as_u16()),
            client_id,
        ));
    }

    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|e| ServiceAccountError::RequestFailed(format!("malformed token response: {e}")))?;
    let ttl = Duration::from_secs(parsed.expires_in.unwrap_or(ASSERTION_TTL_SECS));
    let good_until = Instant::now() + ttl.saturating_sub(EXPIRY_SKEW);
    cache().write().await.insert(
        cache_key,
        CachedToken {
            access_token: parsed.access_token.clone(),
            good_until,
        },
    );
    Ok(parsed.access_token)
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Reduce a token-endpoint error body to its `error` / `error_description`
/// fields. Pure — the raw body can be an HTML error page, so it is never passed
/// through verbatim.
pub fn summarize_token_error(body: &str, status: u16) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let code = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
        let desc = v.get("error_description").and_then(|e| e.as_str()).unwrap_or("");
        if !code.is_empty() {
            return if desc.is_empty() {
                format!("HTTP {status}: {code}")
            } else {
                format!("HTTP {status}: {code} — {}", duduclaw_core::truncate_chars(desc, 200))
            };
        }
    }
    format!("HTTP {status}")
}

/// RS256-sign the claims with the service account's private key.
fn sign_assertion(
    key: &ServiceAccountKey,
    claims: &AssertionClaims,
) -> Result<String, ServiceAccountError> {
    let encoding = jsonwebtoken::EncodingKey::from_rsa_pem(key.private_key.as_bytes())
        // The underlying error can echo PEM bytes; report the shape only.
        .map_err(|e| ServiceAccountError::SigningFailed(e.kind().to_owned_label()))?;
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    jsonwebtoken::encode(&header, claims, &encoding)
        .map_err(|e| ServiceAccountError::SigningFailed(e.kind().to_owned_label()))
}

/// Label a `jsonwebtoken` error kind without echoing any input bytes.
trait ErrorKindLabel {
    fn to_owned_label(&self) -> String;
}

impl ErrorKindLabel for jsonwebtoken::errors::ErrorKind {
    fn to_owned_label(&self) -> String {
        use jsonwebtoken::errors::ErrorKind as K;
        match self {
            K::InvalidKeyFormat => "invalid key format (expected a PKCS#8 PEM RSA private key)",
            K::InvalidRsaKey(_) => "invalid RSA key",
            K::InvalidAlgorithm => "invalid algorithm",
            _ => "signing error",
        }
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_absent_section_is_none() {
        let toml = "[general]\nlog_level = \"info\"\n";
        assert_eq!(parse_config(toml, Path::new("/home")).unwrap(), None);
    }

    #[test]
    fn parse_config_reads_key_file_and_subject() {
        let toml = r#"
[integrations.google_service_account]
key_file = "/keys/sa.json"
subject = "boss@customer.com"
"#;
        let cfg = parse_config(toml, Path::new("/home")).unwrap().unwrap();
        assert_eq!(cfg.key_file, PathBuf::from("/keys/sa.json"));
        assert_eq!(cfg.subject, "boss@customer.com");
    }

    #[test]
    fn parse_config_resolves_relative_key_file_against_home() {
        let toml = r#"
[integrations.google_service_account]
key_file = "sa.json"
subject = "boss@customer.com"
"#;
        let cfg = parse_config(toml, Path::new("/home/.duduclaw")).unwrap().unwrap();
        assert_eq!(cfg.key_file, PathBuf::from("/home/.duduclaw/sa.json"));
    }

    #[test]
    fn parse_config_incomplete_section_errors_instead_of_silently_none() {
        // A typo'd key name must not degrade into "no service account" — that
        // would silently fall back to the OAuth path the operator opted out of.
        let toml = "[integrations.google_service_account]\nsubject = \"boss@customer.com\"\n";
        assert!(matches!(
            parse_config(toml, Path::new("/home")),
            Err(ServiceAccountError::KeyFileIncomplete("key_file"))
        ));
    }

    #[test]
    fn parse_config_rejects_non_email_subject() {
        for bad in ["my-project-id", "boss@localhost", "@customer.com", "a@b@c.com", ""] {
            let toml = format!(
                "[integrations.google_service_account]\nkey_file = \"sa.json\"\nsubject = \"{bad}\"\n"
            );
            assert!(
                matches!(
                    parse_config(&toml, Path::new("/home")),
                    Err(ServiceAccountError::InvalidSubject)
                ),
                "expected rejection for {bad:?}"
            );
        }
    }

    /// The dashboard save path (`google.credentials.set`) and the config-file
    /// path must accept and reject exactly the same values — they now share
    /// [`build_config`] rather than each doing their own checking.
    #[test]
    fn build_config_matches_what_parse_config_accepts() {
        let direct = build_config("k.json", "boss@customer.com", Path::new("/home")).unwrap();
        let viafile = parse_config(
            "[integrations.google_service_account]\nkey_file = \"k.json\"\nsubject = \"boss@customer.com\"\n",
            Path::new("/home"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(direct, viafile);
        assert_eq!(direct.key_file, PathBuf::from("/home/k.json"));
    }

    #[test]
    fn build_config_rejects_the_same_bad_values() {
        assert!(matches!(
            build_config("", "boss@customer.com", Path::new("/home")),
            Err(ServiceAccountError::KeyFileIncomplete("key_file"))
        ));
        assert!(matches!(
            build_config("k.json", "not-an-email", Path::new("/home")),
            Err(ServiceAccountError::InvalidSubject)
        ));
    }

    #[test]
    fn parse_key_happy_path() {
        let raw = r#"{
            "type": "service_account",
            "client_email": "duduclaw@proj.iam.gserviceaccount.com",
            "client_id": "1234567890",
            "private_key": "-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n",
            "token_uri": "https://oauth2.googleapis.com/token"
        }"#;
        let key = parse_key(raw).unwrap();
        assert_eq!(key.client_email, "duduclaw@proj.iam.gserviceaccount.com");
        assert_eq!(key.client_id.as_deref(), Some("1234567890"));
    }

    #[test]
    fn parse_key_rejects_oauth_client_json() {
        // The classic operator mistake: pasting the OAuth client JSON, which has
        // `client_id`/`client_secret` but no `client_email`/`private_key`.
        let raw = r#"{"installed":{"client_id":"x.apps.googleusercontent.com","client_secret":"y"}}"#;
        assert!(matches!(parse_key(raw), Err(ServiceAccountError::KeyFileMalformed(_))));
    }

    #[test]
    fn parse_key_malformed_error_never_quotes_content() {
        // A key file's parse error must not echo bytes — they could be key
        // material. Only a line/column position is reported.
        let raw = "{\"client_email\":\"a@b.com\",\"private_key\":\"SUPERSECRETKEYMATERIAL\",}";
        let err = parse_key(raw).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains("SUPERSECRETKEYMATERIAL"), "leaked key material: {msg}");
        assert!(msg.contains("line"), "expected a position, got: {msg}");
    }

    #[test]
    fn build_claims_shape_matches_the_jwt_bearer_grant() {
        let key = ServiceAccountKey {
            client_email: "sa@proj.iam.gserviceaccount.com".into(),
            private_key: "pem".into(),
            token_uri: None,
            client_id: None,
        };
        let c = build_claims(&key, "boss@customer.com", &["scope.a", "scope.b"], 1_000_000);
        assert_eq!(c.iss, "sa@proj.iam.gserviceaccount.com");
        // `sub` is the impersonated user — the delegation itself. Getting this
        // wrong silently authenticates as the service account instead.
        assert_eq!(c.sub, "boss@customer.com");
        assert_eq!(c.scope, "scope.a scope.b");
        assert_eq!(c.aud, DEFAULT_TOKEN_URI);
        assert_eq!(c.iat, 1_000_000);
        assert_eq!(c.exp, 1_000_000 + ASSERTION_TTL_SECS);
    }

    #[test]
    fn build_claims_honours_key_file_token_uri() {
        let key = ServiceAccountKey {
            client_email: "sa@proj.iam.gserviceaccount.com".into(),
            private_key: "pem".into(),
            token_uri: Some("https://oauth2.example.test/token".into()),
            client_id: None,
        };
        let c = build_claims(&key, "boss@customer.com", &[], 0);
        assert_eq!(c.aud, "https://oauth2.example.test/token");
    }

    #[test]
    fn sign_assertion_rejects_a_bogus_key_without_leaking_it() {
        let key = ServiceAccountKey {
            client_email: "sa@proj.iam.gserviceaccount.com".into(),
            private_key: "-----BEGIN PRIVATE KEY-----\nNOTAREALKEY\n-----END PRIVATE KEY-----".into(),
            client_id: None,
            token_uri: None,
        };
        let claims = build_claims(&key, "boss@customer.com", &["s"], 0);
        let err = sign_assertion(&key, &claims).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ServiceAccountError::SigningFailed(_)));
        assert!(!msg.contains("NOTAREALKEY"), "leaked key bytes: {msg}");
    }

    #[test]
    fn summarize_token_error_extracts_google_error_fields() {
        let body = r#"{"error":"unauthorized_client","error_description":"Client is unauthorized to retrieve access tokens using this method"}"#;
        let s = summarize_token_error(body, 401);
        assert!(s.contains("unauthorized_client"), "{s}");
        assert!(s.contains("401"), "{s}");
    }

    #[test]
    fn summarize_token_error_does_not_pass_through_html() {
        let body = "<html><body>Internal Server Error<script>evil()</script></body></html>";
        let s = summarize_token_error(body, 500);
        assert_eq!(s, "HTTP 500");
    }

    #[test]
    fn rejected_error_names_the_client_id_to_authorize() {
        // The operator's next action is pasting this id into Admin console, so
        // the error must hand it to them.
        let e = ServiceAccountError::Rejected("HTTP 401: unauthorized_client".into(), "1234567890".into());
        let msg = e.to_string();
        assert!(msg.contains("1234567890"), "{msg}");
        assert!(msg.contains("Domain Wide Delegation"), "{msg}");
    }
}
