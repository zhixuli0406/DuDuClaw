//! SourceOwner lexical child: only this private sender opens the captured
//! raw client. It returns encrypted candidate + metadata CAS, never a client,
//! bearer, request/header builder or raw secret accessor.
use super::*;
use crate::mcp_oauth::{home_lifecycle, home_store::PreparedGoogleCallback};
use std::time::Duration;
use zeroize::Zeroizing;
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_TOKEN: usize = 1024 * 1024;
enum GoogleScopeEvidence {
    ResponseScope,
    CapturedRequestOmission,
}
struct ResponseTokens {
    access: Zeroizing<String>,
    refresh: Option<Zeroizing<String>>,
    expires_at: chrono::DateTime<chrono::Utc>,
    scopes: Vec<String>,
    evidence: GoogleScopeEvidence,
}
fn expected_request(requested: &[String]) -> Result<Vec<String>> {
    let actual = home_store::normalized_scopes(requested.to_vec())?;
    let required = home_store::normalized_scopes(
        crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    )?;
    if actual != required {
        return Err(Error::Invalid);
    }
    Ok(actual)
}
fn bearer(value: &str) -> bool {
    let mut split = value.splitn(2, '=');
    let prefix = split.next().unwrap_or("");
    !prefix.is_empty()
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/".contains(&b))
        && split.next().is_none_or(|s| s.bytes().all(|b| b == b'='))
}
fn parse_tokens(body: Value, requested: &[String], now: u64) -> Result<ResponseTokens> {
    let requested = expected_request(requested)?;
    let access = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= MAX_TOKEN && bearer(s))
        .ok_or(Error::Invalid)?;
    if !body
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
    {
        return Err(Error::Invalid);
    }
    let refresh = match body.get("refresh_token") {
        None => None,
        Some(Value::String(s))
            if !s.is_empty() && s.len() <= MAX_TOKEN && !s.chars().any(char::is_control) =>
        {
            Some(Zeroizing::new(s.clone()))
        }
        _ => return Err(Error::Invalid),
    };
    let secs = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|s| *s > 0)
        .and_then(|s| i64::try_from(s).ok())
        .and_then(chrono::Duration::try_seconds)
        .ok_or(Error::Invalid)?;
    let expires_at =
        chrono::DateTime::from_timestamp(i64::try_from(now).map_err(|_| Error::Invalid)?, 0)
            .and_then(|n| n.checked_add_signed(secs))
            .ok_or(Error::Invalid)?;
    let (scopes, evidence) = match body.get("scope") {
        None => (
            requested.clone(),
            GoogleScopeEvidence::CapturedRequestOmission,
        ),
        Some(Value::String(scope)) if !scope.is_empty() && scope.len() <= 128 * 2049 => {
            let pieces: Vec<_> = scope.split(' ').map(str::to_owned).collect();
            if pieces.iter().any(|s| {
                s.bytes().any(|b| {
                    !(b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
                })
            }) {
                return Err(Error::Invalid);
            }
            (
                home_store::normalized_scopes(pieces)?,
                GoogleScopeEvidence::ResponseScope,
            )
        }
        _ => return Err(Error::Invalid),
    };
    if requested.iter().any(|s| !scopes.contains(s)) {
        return Err(Error::Invalid);
    }
    Ok(ResponseTokens {
        access: Zeroizing::new(access.to_owned()),
        refresh,
        expires_at,
        scopes,
        evidence,
    })
}
fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| Error::Io)
}
fn endpoint(_home: &Path) -> String {
    #[cfg(test)]
    if let Some(url) = testing::endpoint(_home) {
        return url;
    }
    TOKEN_ENDPOINT.to_owned()
}
fn validate_captured(
    home: &Path,
    claim: &home_lifecycle::GoogleHomeCallbackContext,
    original: &home_store::StoreSnapshot,
) -> Result<()> {
    home_lifecycle::validate_google_home_claim(home, claim)?;
    with_metadata(home, |g, source| {
        claim.validate_deadlines(home)?;
        home_store::validate_callback_snapshot(g, source, claim.durable(), original)
    })
}
pub(crate) async fn exchange_claimed_google_code_once(
    home: &Path,
    claim: &home_lifecycle::GoogleHomeCallbackContext,
    code: &str,
    verifier: &str,
    requested: &[String],
) -> Result<PreparedGoogleCallback> {
    expected_request(requested)?;
    home_lifecycle::validate_google_home_claim(home, claim)?;
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, -2);
    let original = with_metadata(home, |g, source| {
        claim.validate_deadlines(home)?;
        home_store::callback_control_snapshot(g, source, claim.durable())
    })?;
    // SourceOwner reads its own private field. Owned clones come ONLY from
    // this original claim; no latest client lookup or borrowed lock survives.
    let raw = &claim.durable().captured_client()._raw;
    validate_google_client_raw(raw)?;
    if raw["token_url"] != TOKEN_ENDPOINT
        || raw["auth_url"] != "https://accounts.google.com/o/oauth2/v2/auth"
    {
        return Err(Error::Invalid);
    }
    let saved_scopes: Vec<String> =
        serde_json::from_value(raw["scopes"].clone()).map_err(|_| Error::Invalid)?;
    if home_store::normalized_scopes(saved_scopes)? != expected_request(requested)? {
        return Err(Error::Stale);
    }
    let client_id = raw["client_id"].as_str().ok_or(Error::Invalid)?.to_owned();
    let redirect = raw["redirect_uri"]
        .as_str()
        .ok_or(Error::Invalid)?
        .to_owned();
    let stored = raw["client_secret"].as_str().ok_or(Error::Invalid)?;
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, -7);
    validate_captured(home, claim, &original)?;
    let secret = if stored.is_empty() {
        Zeroizing::new(String::new())
    } else {
        let cipher = stored.strip_prefix("enc:v1:").ok_or(Error::Invalid)?;
        #[cfg(test)]
        {
            home_store::test_probe::crypto(home);
            home_store::test_probe::google_crypto(home);
        }
        Zeroizing::new(crate::config_crypto::decrypt_value(cipher, home).ok_or(Error::Io)?)
    };
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, -3);
    validate_captured(home, claim, &original)?;
    let sender = client()?;
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, -4);
    validate_captured(home, claim, &original)?;
    #[cfg(test)]
    home_store::test_probe::provider(&client_id);
    let sent = sender
        .post(endpoint(home))
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", client_id.as_str()),
            ("client_secret", secret.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await;
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, -5);
    validate_captured(home, claim, &original)?;
    let mut response = sent.map_err(|_| Error::Io)?;
    if !response.status().is_success() {
        return Err(Error::Io);
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(Error::Invalid);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        validate_captured(home, claim, &original)?;
        let chunk = response.chunk().await;
        #[cfg(test)]
        home_lifecycle::testing::finish_boundary(home, -6);
        validate_captured(home, claim, &original)?;
        let Some(chunk) = chunk.map_err(|_| Error::Io)? else {
            break;
        };
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|n| n > MAX_BODY)
        {
            return Err(Error::Invalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    let parsed = parse_tokens(
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?,
        requested,
        home_store::wall_time_now(home),
    )?;
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, -1);
    with_metadata(home, |g, source| {
        home_store::preflight_callback_payload(
            g,
            source,
            claim.durable(),
            &original,
            parsed.access.len(),
            parsed.refresh.as_ref().map(|s| s.len()),
            parsed.expires_at,
            &parsed.scopes,
        )
    })?;
    validate_captured(home, claim, &original)?;
    fn seal(home: &Path, s: &str) -> Result<String> {
        #[cfg(test)]
        {
            home_store::test_probe::crypto(home);
            home_store::test_probe::google_crypto(home);
        }
        crate::config_crypto::encrypt_value(s, home)
            .map(|v| format!("enc:v1:{v}"))
            .ok_or(Error::Io)
    }
    let _scope_evidence = parsed.evidence;
    validate_captured(home, claim, &original)?;
    let access = seal(home, &parsed.access)?;
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, -8);
    validate_captured(home, claim, &original)?;
    let refresh = if let Some(refresh) = &parsed.refresh {
        #[cfg(test)]
        home_lifecycle::testing::finish_boundary(home, -9);
        validate_captured(home, claim, &original)?;
        let sealed = seal(home, refresh)?;
        #[cfg(test)]
        home_lifecycle::testing::finish_boundary(home, -10);
        validate_captured(home, claim, &original)?;
        Some(sealed)
    } else {
        None
    };
    let token = crate::mcp_oauth::McpOAuthToken {
        provider_id: "google".into(),
        access_token: access,
        refresh_token: refresh,
        expires_at: Some(parsed.expires_at),
        scopes: parsed.scopes,
    };
    #[cfg(test)]
    home_lifecycle::testing::finish_boundary(home, 0);
    validate_captured(home, claim, &original)?;
    home_store::encrypted_callback_candidate(token, original)
}
#[cfg(test)]
#[path = "home_callback_test_wire.rs"]
pub(crate) mod testing;
#[cfg(test)]
#[path = "home_callback_io_tests.rs"]
mod tests;
