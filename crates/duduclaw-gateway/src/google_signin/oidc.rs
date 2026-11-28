//! Strict Google OIDC verification. Claims are trusted only after signature,
//! issuer, audience, presenter, time and nonce checks; no provider text is logged.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::google_personal::oauth;

const ISSUERS: &[&str] = &["accounts.google.com", "https://accounts.google.com"];
const LEEWAY_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigninPurpose { Login, Reauth }

/// The caller binds state, nonce and verifier to its one-time pending flow.
pub fn authorization_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
    code_challenge_s256: &str,
    allowed_hd: &[String],
    purpose: SigninPurpose,
    login_hint: Option<&str>,
) -> String {
    // Wildcard hd is not documented in Google's docs; the server-side hd check is the control.
    // An empty allowlist still sends hd, but verification cannot authorize anyone.
    let hd = match allowed_hd { [domain] => domain.as_str(), _ => "*" };
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.extend_pairs([
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", "openid email profile"),
        ("state", state),
        ("nonce", nonce),
        ("code_challenge", code_challenge_s256),
        ("code_challenge_method", "S256"),
        ("access_type", "online"),
        ("prompt", "select_account"),
        ("hd", hd),
    ]);
    if purpose == SigninPurpose::Reauth {
        query.append_pair("claims", r#"{"id_token":{"auth_time":{"essential":true}}}"#);
        // Google does not document max_age; reauth_fresh is the enforcement.
        query.append_pair("max_age", "0");
    }
    if let Some(hint) = login_hint { query.append_pair("login_hint", hint); }
    format!("{}?{}", oauth::endpoint(oauth::AUTHORIZE), query.finish())
}

fn random_value() -> Result<String, IdTokenError> {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes).map_err(|_| IdTokenError::Invalid)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn random_state() -> Result<String, IdTokenError> { random_value() }
pub fn random_nonce() -> Result<String, IdTokenError> { random_value() }
pub fn random_pkce_verifier() -> Result<String, IdTokenError> { random_value() }

pub fn pkce_challenge_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// Deliberately no Debug implementation: identity claims must not enter logs.
pub struct VerifiedIdToken {
    pub sub: String,
    pub email: String,
    pub hd: String,
    pub name: String,
    pub auth_time: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdTokenError { Invalid, PersonalAccount, DomainNotAllowed }

/// Validate against the caller's Unix timestamp (one clock for all checks).
/// No domain or identity value is inferred from hints or email suffixes.
pub async fn verify_google_id_token(
    http: &reqwest::Client,
    id_token: &str,
    client_id: &str,
    expected_nonce_sha256: &[u8; 32],
    allowed_hd: &[String],
    now: i64,
) -> Result<VerifiedIdToken, IdTokenError> {
    if id_token.is_empty() || id_token.len() > 16 * 1024 || client_id.is_empty() {
        return Err(IdTokenError::Invalid);
    }
    let claims = crate::webhook_jwt::verify_rs256_any_issuer_at(
        http, id_token, &oauth::endpoint(oauth::GOOGLE_JWKS), ISSUERS,
        client_id, LEEWAY_SECS, now,
    ).await.map_err(|_| IdTokenError::Invalid)?;
    // jsonwebtoken accepts audience arrays; Google sign-in explicitly does not.
    if claims.get("aud").and_then(Value::as_str) != Some(client_id)
        || !claims.get("iss").and_then(Value::as_str).is_some_and(|iss| ISSUERS.contains(&iss))
        || claims.get("azp").is_some_and(|azp| azp.as_str() != Some(client_id))
    {
        return Err(IdTokenError::Invalid);
    }
    let iat = claims.get("iat").and_then(Value::as_i64).ok_or(IdTokenError::Invalid)?;
    if i128::from(iat) > i128::from(now) + i128::from(LEEWAY_SECS) {
        return Err(IdTokenError::Invalid);
    }
    let nonce = claims.get("nonce").and_then(Value::as_str).ok_or(IdTokenError::Invalid)?;
    let digest: [u8; 32] = Sha256::digest(nonce.as_bytes()).into();
    ring::constant_time::verify_slices_are_equal(&digest, expected_nonce_sha256)
        .map_err(|_| IdTokenError::Invalid)?;

    let sub = claims.get("sub").and_then(Value::as_str)
        .filter(|sub| duduclaw_auth::google_signin::valid_google_sub(sub))
        .ok_or(IdTokenError::Invalid)?;
    let email = claims.get("email").and_then(Value::as_str)
        .filter(|email| valid_email_shape(email)).ok_or(IdTokenError::Invalid)?;
    let name = match claims.get("name") {
        None => "",
        Some(value) => value.as_str().ok_or(IdTokenError::Invalid)?,
    };
    let auth_time = claims.get("auth_time")
        .map(|value| value.as_i64().ok_or(IdTokenError::Invalid)).transpose()?;

    // Classify account/domain failures only after all authentication and
    // identity-shape checks pass. A present but malformed hd is never personal.
    let hd = claims.get("hd").ok_or(IdTokenError::PersonalAccount)?
        .as_str().ok_or(IdTokenError::DomainNotAllowed)?.to_ascii_lowercase();
    if hd.is_empty() || !hd.bytes().all(domain_byte)
        || !allowed_hd.iter().any(|allowed| allowed == &hd)
        || claims.get("email_verified") != Some(&Value::Bool(true))
    {
        return Err(IdTokenError::DomainNotAllowed);
    }
    Ok(VerifiedIdToken { sub: sub.to_owned(), email: email.to_owned(), hd,
        name: name.to_owned(), auth_time })
}

// Match the member provisioning email-shape contract without treating its
// domain as authorization. The auth crate's validator is crate-private.
fn valid_email_shape(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else { return false; };
    email.len() <= 254 && !local.is_empty() && !domain.is_empty()
        && !domain.contains('@')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, ',' | ';' | '<' | '>' | '"'))
        && domain.split('.').all(|label| !label.is_empty())
        && domain.split('.').count() >= 2
}

pub fn reauth_fresh(token: &VerifiedIdToken, now: i64) -> bool {
    token.auth_time.is_some_and(|at| {
        let at = i128::from(at);
        let now = i128::from(now);
        now - 300 <= at && at <= now + 60
    })
}

fn domain_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-')
}

pub fn normalize_allowed_hd(input: &str) -> Option<String> {
    // Reject non-ASCII/IDNA input rather than converting Unicode domain names.
    let domain = input.trim().to_ascii_lowercase();
    if domain.is_empty() || domain.len() > 253 || !domain.bytes().all(domain_byte)
        || domain.starts_with(['.', '-']) || domain.ends_with(['.', '-'])
        || domain.contains("..")
    {
        return None;
    }
    Some(domain)
}
