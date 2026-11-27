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
pub(crate) const DRIVE_WRITE_SCOPE: &str = "https://www.googleapis.com/auth/drive";
pub const GOOGLE_JWKS: &str = "https://www.googleapis.com/oauth2/v3/certs";
pub(crate) const DRIVE_TOOLS_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
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

#[cfg(test)]
tokio::task_local! {
    /// Per-test override; never changes the process-wide Personal OAuth mock.
    pub(crate) static TEST_ENDPOINT_BASE: String;
}

pub(crate) fn endpoint(url: &str) -> String {
    #[cfg(test)]
    if let Ok(base) = TEST_ENDPOINT_BASE.try_with(Clone::clone) {
        return format!("{}/{}", base.trim_end_matches('/'), url.trim_start_matches("https://"));
    }
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

/// Pinned refresh must make one transport attempt, including protocol NACKs.
/// The ordinary OAuth client retains its existing transport policy.
enum ScopedRefreshPolicy<'a> {
    Legacy,
    Meeting,
    Closed {
        home: &'a std::path::Path,
        deadline: tokio::time::Instant,
    },
}
impl ScopedRefreshPolicy<'_> {
    fn check(&self) -> Result<(), Error> {
        duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|_| Error::Unavailable)?;
        if let Self::Closed { deadline, .. } = self {
            if tokio::time::Instant::now() >= *deadline {
                return Err(Error::Unavailable);
            }
        }
        Ok(())
    }
    async fn wait<F: std::future::Future>(&self, future: F) -> Result<F::Output, Error> {
        self.check()?;
        let value = match self {
            Self::Legacy | Self::Meeting => future.await,
            Self::Closed { deadline, .. } => tokio::time::timeout_at(*deadline, future)
                .await
                .map_err(|_| Error::Unavailable)?,
        };
        self.check()?;
        Ok(value)
    }
    fn endpoint(&self) -> Result<String, Error> {
        match self {
            Self::Legacy => Ok(endpoint(TOKEN)),
            Self::Meeting => {
                #[cfg(test)]
                {
                    return test_base().get().map(|base| format!("{}/oauth2.googleapis.com/token", base.trim_end_matches('/')))
                        .ok_or(Error::Unavailable);
                }
                #[cfg(not(test))]
                Ok(TOKEN.to_owned())
            }
            Self::Closed { home, .. } => {
                #[cfg(test)]
                {
                    return mcp_refresh_test::endpoint(home).ok_or(Error::Unavailable);
                }
                #[cfg(not(test))]
                {
                    let _ = home;
                    Ok(TOKEN.to_owned())
                }
            }
        }
    }
}
fn scoped_refresh_client(policy: &ScopedRefreshPolicy<'_>) -> Result<reqwest::Client, Error> {
    let builder = match policy {
        ScopedRefreshPolicy::Legacy if duduclaw_mcp_boundary::io_authority::is_scoped() => reqwest::Client::builder().no_proxy(),
        ScopedRefreshPolicy::Legacy => reqwest::Client::builder(),
        ScopedRefreshPolicy::Meeting | ScopedRefreshPolicy::Closed { .. } => reqwest::Client::builder().no_proxy(),
    };
    builder
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| Error::Unavailable)
}

async fn response_json(mut response: reqwest::Response) -> Result<Value, Error> {
    let status = response.status();
    // v1.81 E3: redirects are never followed (Policy::none) and a 3xx is a
    // failure that is never parsed, so a redirect body can never be read as
    // tokens or as an `invalid_grant` consent-revoke signal.
    if status.is_redirection() {
        return Err(Error::Rejected);
    }
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

/// Sign-in needs only the ID token. Never construct a vault Secret or retain
/// access/refresh tokens, and never include provider response text in errors.
pub async fn exchange_for_id_token(
    client_id: &str,
    client_secret: &str,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<String, Error> {
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
    let mut value = response_json(response).await?;
    let id_token = value.as_object_mut()
        .and_then(|object| object.remove("id_token"));
    // Drop all other response fields before validating or returning the token.
    drop(value);
    let Some(Value::String(id_token)) = id_token else {
        return Err(Error::Malformed);
    };
    if id_token.is_empty() || id_token.len() > 16 * 1024 {
        return Err(Error::Malformed);
    }
    let mut parts = id_token.split('.');
    if !(0..3).all(|_| parts.next().is_some_and(|part| !part.is_empty())) || parts.next().is_some() {
        return Err(Error::Malformed);
    }
    Ok(id_token)
}

#[cfg(test)]
tokio::task_local! { pub(crate) static TEST_REFRESH_URL: String; }

pub async fn refresh(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Tokens, Error> {
    let token_url = endpoint(TOKEN);
    #[cfg(test)]
    let token_url = TEST_REFRESH_URL.try_with(Clone::clone).unwrap_or(token_url);
    let response = client()?
        .post(token_url)
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

#[cfg(test)]
tokio::task_local! { pub(crate) static TEST_TOKENINFO_URL: String; }

/// Connection diagnostics use only OAuth metadata, never a Workspace API.
pub(crate) async fn tokeninfo(access_token: &str, client_id: &str) -> Result<Vec<String>, Error> {
    let tokeninfo_url = endpoint("https://oauth2.googleapis.com/tokeninfo");
    #[cfg(test)]
    let tokeninfo_url = TEST_TOKENINFO_URL.try_with(Clone::clone).unwrap_or(tokeninfo_url);
    let response = client()?
        .get(tokeninfo_url)
        .query(&[("access_token", access_token)])
        .send().await.map_err(|_| Error::Unavailable)?;
    let value = response_json(response).await?;
    if value.get("aud").and_then(serde_json::Value::as_str) != Some(client_id) {
        return Err(Error::Unavailable);
    }
    let scopes = value.get("scope").and_then(serde_json::Value::as_str).ok_or(Error::Unavailable)?;
    Ok(scopes.split_whitespace().map(str::to_owned).collect())
}

pub(crate) async fn refresh_scoped(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Tokens, Error> {
    refresh_scoped_core(
        client_id,
        client_secret,
        refresh_token,
        ScopedRefreshPolicy::Legacy,
    )
    .await
}
pub(crate) async fn refresh_meeting(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<Tokens, Error> {
    refresh_scoped_core(client_id, client_secret, refresh_token, ScopedRefreshPolicy::Meeting).await
}
pub(crate) async fn refresh_mcp(
    home: &std::path::Path,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    deadline: tokio::time::Instant,
) -> Result<Tokens, Error> {
    refresh_scoped_core(
        client_id,
        client_secret,
        refresh_token,
        ScopedRefreshPolicy::Closed { home, deadline },
    )
    .await
}
async fn refresh_scoped_core(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    policy: ScopedRefreshPolicy<'_>,
) -> Result<Tokens, Error> {
    policy.check()?;
    let sender = scoped_refresh_client(&policy)?;
    let request = sender
        .post(policy.endpoint()?)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
        ])
        .build()
        .map_err(|_| Error::Unavailable)?;
    policy.check()?;
    let response = policy
        .wait(sender.execute(request))
        .await?
        .map_err(|_| Error::Unavailable)?;
    let result = policy.wait(response_json(response)).await?;
    policy.check()?;
    parse_tokens(result?)
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
    authorization_url_with_meetings(client_id, redirect, state, challenge, false)
}

pub fn authorization_url_with_meetings(
    client_id: &str,
    redirect: &str,
    state: &str,
    challenge: &str,
    meeting_recordings: bool,
) -> Result<String, String> {
    authorization_url_for(client_id, redirect, state, challenge, meeting_recordings, false, false, false)
}

pub fn authorization_url_with_drive_tools(
    client_id: &str,
    redirect: &str,
    state: &str,
    challenge: &str,
) -> Result<String, String> {
    authorization_url_for(client_id, redirect, state, challenge, false, true, false, false)
}

pub(crate) fn authorization_url_for(
    client_id: &str,
    redirect: &str,
    state: &str,
    challenge: &str,
    meeting_recordings: bool,
    drive_tools: bool,
    chat_read: bool,
    chat_send: bool,
) -> Result<String, String> {
    let purpose = if chat_send { "chat_send" } else if drive_tools { "drive_tools" }
        else if meeting_recordings { "meeting_recordings" } else { "base" };
    authorization_url_for_scopes(client_id, redirect, state, challenge,
        &scopes_for_purposes(&[purpose], chat_read))
}

pub(crate) fn authorization_url_for_scopes(
    client_id: &str, redirect: &str, state: &str, challenge: &str, scopes: &[&str],
) -> Result<String, String> {
    let mut url = url::Url::parse(&endpoint(AUTHORIZE)).map_err(|_| "Google 連結網址無法建立。")?;
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
        ("include_granted_scopes", "false"),
    ]);
    Ok(url.into())
}

/// Explicit local consent union; never inherit provider-side project grants.
pub(crate) fn scopes_for_purposes(purposes: &[&str], chat_read: bool) -> Vec<&'static str> {
    let mut scopes = vec!["openid", "email"];
    for purpose in purposes {
        match *purpose {
            "base" => scopes.extend(requested_scopes(false, false)),
            "drive_tools" => scopes.push(DRIVE_TOOLS_SCOPE),
            "meeting_recordings" => scopes.extend(requested_scopes(true, false)),
            "chat_read" => scopes.extend_from_slice(crate::google_workspace::CHAT_READ_SCOPES),
            "chat_send" => scopes.extend_from_slice(crate::google_workspace::CHAT_SEND_SCOPES),
            _ => {},
        }
    }
    if chat_read { scopes.extend_from_slice(crate::google_workspace::CHAT_READ_SCOPES); }
    scopes.sort_unstable();
    scopes.dedup();
    scopes
}

pub(crate) fn requested_scopes(meeting_recordings: bool, drive_tools: bool) -> Vec<&'static str> {
    let mut scopes: Vec<&str> = if drive_tools {
        vec![DRIVE_TOOLS_SCOPE]
    } else if meeting_recordings {
        vec![super::meeting_access::DRIVE_SCOPE, super::meeting_access::MEET_SCOPE]
    } else {
        crate::google_workspace::REQUIRED_SCOPES.iter().copied()
            .filter(|scope| *scope != DRIVE_TOOLS_SCOPE)
            .collect()
    };
    scopes.extend(["openid", "email"]);
    scopes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_scope_union_is_sorted_deduplicated_and_keeps_chat_read() {
        let scopes = scopes_for_purposes(&["base", "chat_send", "drive_tools", "base"], true);
        assert!(scopes.windows(2).all(|pair| pair[0] < pair[1]));
        for scope in requested_scopes(false, false).into_iter()
            .chain(crate::google_workspace::CHAT_SEND_SCOPES.iter().copied())
            .chain(crate::google_workspace::CHAT_READ_SCOPES.iter().copied())
            .chain([DRIVE_TOOLS_SCOPE]) {
            assert!(scopes.contains(&scope), "{scope}");
        }
        assert!(!scopes.contains(&DRIVE_WRITE_SCOPE));
    }

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
            ("include_granted_scopes", "false"),
            ("prompt", "consent"),
            ("state", "state-fixture"),
            ("code_challenge_method", "S256"),
            ("code_challenge", "challenge-fixture"),
        ] {
            assert_eq!(q.get(key).unwrap(), value);
        }
        assert!(q["scope"].split_ascii_whitespace().any(|s| s == "openid"));
    }

    #[test]
    fn meeting_consent_is_incremental_and_not_part_of_initial_connect() {
        let initial = authorization_url("client", "https://dashboard.example/callback", "state", "challenge").unwrap();
        let incremental = authorization_url_with_meetings("client", "https://dashboard.example/callback", "state", "challenge", true).unwrap();
        let drive_tools = authorization_url_with_drive_tools("client", "https://dashboard.example/callback", "state", "challenge").unwrap();
        let scopes = |raw: &str| {
            url::Url::parse(raw).unwrap().query_pairs().find(|(key, _)| key == "scope")
                .unwrap().1.split_ascii_whitespace().map(str::to_owned).collect::<Vec<_>>()
        };
        let initial_scopes = scopes(&initial);
        for withheld in [super::super::meeting_access::DRIVE_SCOPE, super::super::meeting_access::MEET_SCOPE, "https://www.googleapis.com/auth/drive.readonly"] {
            assert!(!initial_scopes.iter().any(|s| s == withheld));
        }
        let incremental_scopes = scopes(&incremental);
        assert_eq!(incremental_scopes.len(), 4);
        assert!(super::super::meeting_access::has_scopes(&incremental_scopes));
        assert!(!incremental_scopes.iter().any(|scope| scope == DRIVE_TOOLS_SCOPE));
        let drive_scopes = scopes(&drive_tools);
        assert_eq!(drive_scopes.len(), 3);
        assert!(drive_scopes.iter().any(|scope| scope == DRIVE_TOOLS_SCOPE));
        assert!(!drive_scopes.iter().any(|scope| scope == super::super::meeting_access::DRIVE_SCOPE || scope == super::super::meeting_access::MEET_SCOPE));
        assert!(url::Url::parse(&drive_tools).unwrap().query_pairs().any(|(key, value)| key == "include_granted_scopes" && value == "false"));
        assert!(url::Url::parse(&incremental).unwrap().query_pairs().any(|(key, value)| key == "include_granted_scopes" && value == "false"));
    }
}

#[cfg(test)]
pub(crate) mod mcp_refresh_test {
    use std::{
        path::{Path, PathBuf},
        collections::HashMap,
        sync::{Mutex, OnceLock},
    };
    fn fixtures() -> &'static Mutex<HashMap<PathBuf, String>> {
        static F: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
        F.get_or_init(Default::default)
    }
    pub(crate) struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            fixtures().lock().unwrap().remove(&self.0);
        }
    }
    pub(crate) fn install(home: &Path, endpoint: &str) -> Guard {
        let url = reqwest::Url::parse(endpoint).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert!(url.port().is_some());
        let home = home.canonicalize().unwrap();
        assert!(
            fixtures()
                .lock()
                .unwrap()
                .insert(home.clone(), endpoint.into())
                .is_none()
        );
        Guard(home)
    }
    pub(crate) fn endpoint(home: &Path) -> Option<String> {
        fixtures()
            .lock()
            .unwrap()
            .get(&home.canonicalize().ok()?)
            .cloned()
    }
}
