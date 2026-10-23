//! Google OAuth transport. Fixed Google endpoints in production, no
//! redirects, bounded responses and no provider body in errors or logs.

use crate::person_credentials::Secret;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

pub const AUTHORIZE: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN: &str = "https://oauth2.googleapis.com/token";
pub const USERINFO: &str = "https://openidconnect.googleapis.com/v1/userinfo";
pub const REVOKE: &str = "https://oauth2.googleapis.com/revoke";
const MAX_RESPONSE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidGrant,
    Rejected,
    Unavailable,
    Malformed,
}

pub struct Tokens {
    pub secret: Secret,
    pub granted_scopes: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct UserInfo {
    pub sub: String,
    pub email: String,
    #[serde(default)]
    pub email_verified: bool,
    pub hd: Option<String>,
}

#[cfg(test)]
pub(crate) fn test_base() -> &'static std::sync::OnceLock<String> {
    static BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    &BASE
}

fn endpoint(url: &str) -> String {
    #[cfg(test)]
    if let Some(base) = test_base().get() {
        return format!(
            "{}/{}",
            base.trim_end_matches('/'),
            url.trim_start_matches("https://")
        );
    }
    #[cfg(feature = "live-test-mocks")]
    if let Ok(base) = std::env::var("DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE") {
        return format!(
            "{}/{}",
            base.trim_end_matches('/'),
            url.trim_start_matches("https://")
        );
    }
    url.to_owned()
}

fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::Unavailable)
}

async fn response_json(mut response: reqwest::Response) -> Result<Value, Error> {
    let status = response.status();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unavailable)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(Error::Malformed);
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Malformed)?;
    if !status.is_success() {
        return Err(
            if value.get("error").and_then(Value::as_str) == Some("invalid_grant") {
                Error::InvalidGrant
            } else {
                Error::Rejected
            },
        );
    }
    Ok(value)
}

fn parse_tokens(value: Value) -> Result<Tokens, Error> {
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 16 * 1024)
        .ok_or(Error::Malformed)?
        .to_owned();
    if !value
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|v| v.eq_ignore_ascii_case("bearer"))
    {
        return Err(Error::Malformed);
    }
    let seconds = value
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0 && *n <= 365 * 86400)
        .ok_or(Error::Malformed)?;
    let expires_at = chrono::Utc::now()
        .timestamp()
        .checked_add(seconds)
        .ok_or(Error::Malformed)?;
    let refresh_token = value
        .get("refresh_token")
        .map(|v| {
            v.as_str()
                .filter(|v| !v.is_empty() && v.len() <= 16 * 1024)
                .map(str::to_owned)
                .ok_or(Error::Malformed)
        })
        .transpose()?;
    let granted_scopes = value
        .get("scope")
        .map(|v| {
            let text = v
                .as_str()
                .filter(|s| s.len() <= 16 * 1024)
                .ok_or(Error::Malformed)?;
            let scopes: Vec<String> = text.split_ascii_whitespace().map(str::to_owned).collect();
            if scopes.len() > 128 {
                return Err(Error::Malformed);
            }
            Ok(scopes)
        })
        .transpose()?;
    Ok(Tokens {
        secret: Secret {
            access_token,
            refresh_token,
            expires_at: Some(expires_at),
        },
        granted_scopes,
    })
}

/// The client id/secret comes from the Admin's provider configuration;
/// endpoints never come from a callback, token body or user argument.
pub async fn exchange(
    client_id: &str,
    client_secret: &str,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<Tokens, Error> {
    let response = client()?
        .post(endpoint(TOKEN))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect),
        ])
        .send()
        .await
        .map_err(|_| Error::Unavailable)?;
    parse_tokens(response_json(response).await?)
}

pub async fn refresh(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Tokens, Error> {
    let response = client()?
        .post(endpoint(TOKEN))
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|_| Error::Unavailable)?;
    parse_tokens(response_json(response).await?)
}

pub async fn userinfo(access_token: &str) -> Result<UserInfo, Error> {
    let response = client()?
        .get(endpoint(USERINFO))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| Error::Unavailable)?;
    let mut info: UserInfo =
        serde_json::from_value(response_json(response).await?).map_err(|_| Error::Malformed)?;
    if !info.email_verified
        || info.sub.is_empty()
        || info.sub.len() > 255
        || info.sub.chars().any(char::is_control)
    {
        return Err(Error::Malformed);
    }
    info.email = duduclaw_core::google_identity::normalize_email(&info.email)
        .map_err(|_| Error::Malformed)?;
    if let Some(domain) = &mut info.hd {
        *domain = domain.trim().to_ascii_lowercase();
        if !duduclaw_core::google_identity::is_domain(domain) {
            return Err(Error::Malformed);
        }
    }
    Ok(info)
}

pub async fn revoke(token: &str) -> bool {
    let Ok(client) = client() else {
        return false;
    };
    client
        .post(endpoint(REVOKE))
        .form(&[("token", token)])
        .send()
        .await
        .is_ok_and(|r| r.status() == reqwest::StatusCode::OK)
}

pub fn authorization_url(
    client_id: &str,
    redirect: &str,
    state: &str,
    challenge: &str,
) -> Result<String, String> {
    let mut url = url::Url::parse(&endpoint(AUTHORIZE)).map_err(|_| "Google 連結網址無法建立。")?;
    let mut scopes: Vec<&str> = crate::google_workspace::REQUIRED_SCOPES.to_vec();
    scopes.extend(["openid", "email"]);
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("scope", scopes.join(" ").as_str()),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("include_granted_scopes", "true"),
    ]);
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_scope_is_actual_not_requested_and_malformed_tokens_refuse() {
        let mut value = serde_json::json!({"access_token":"fixture", "token_type":"Bearer", "expires_in":3600, "scope":"openid email"});
        assert_eq!(
            parse_tokens(value.clone()).unwrap().granted_scopes.unwrap(),
            ["openid", "email"]
        );
        value["expires_in"] = serde_json::json!(-1);
        assert!(parse_tokens(value).is_err());
    }

    #[test]
    fn authorization_binds_offline_consent_state_and_pkce() {
        let url = authorization_url(
            "client",
            "https://dashboard.example/api/me/google/callback",
            "state-fixture",
            "challenge-fixture",
        )
        .unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let q = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        for (key, value) in [
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", "state-fixture"),
            ("code_challenge_method", "S256"),
            ("code_challenge", "challenge-fixture"),
        ] {
            assert_eq!(q.get(key).unwrap(), value);
        }
        assert!(q["scope"].split_ascii_whitespace().any(|s| s == "openid"));
    }
}
