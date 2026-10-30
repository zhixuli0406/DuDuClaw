//! Account login using the REST shapes in gateway/server.rs, never admin tokens.
use crate::{Error, Result, sanitize::TerminalText, strings, transport::GatewayUrl};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, Zeroizing};

/// Implementations must disable echo for Password and Otp, and never log input.
/// Phase 3 supplies the terminal implementation; tests supply scripted answers.
#[async_trait]
pub trait LoginPrompt: Send {
    async fn read(&mut self, field: PromptField) -> Result<Zeroizing<String>>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptField {
    Email,
    Password,
    Otp,
}
impl PromptField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Email => strings::EMAIL,
            Self::Password => strings::PASSWORD,
            Self::Otp => strings::OTP,
        }
    }
    pub fn secret(self) -> bool {
        self != Self::Email
    }
}

#[derive(Clone, Copy, Debug)]
pub enum LoginMode {
    Personal,
    TeamOrEnterprise,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Manager,
    Employee,
}
impl Role {
    pub fn admin_panel_notice(self) -> Option<&'static str> {
        (self != Self::Admin).then_some(strings::ADMIN_ONLY)
    }
}
#[derive(Clone, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: TerminalText,
    pub role: Role,
    #[serde(default)]
    pub must_change_password: bool,
}
impl Account {
    pub(crate) fn validate(&self) -> Result<()> {
        if matches!(self.id.as_str(), "system" | "admin_fallback") {
            return Err(Error::LegacyIdentity);
        }
        if self.id.is_empty() {
            return Err(Error::Protocol);
        }
        if self.must_change_password {
            return Err(Error::PasswordChangeRequired);
        }
        Ok(())
    }
}

/// No Debug or Serialize implementation: tokens cannot leak through diagnostics.
pub struct Session {
    pub account: Account,
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
    expires_at: u64,
}
impl Session {
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }
    pub fn refresh_in(&self, margin: Duration) -> Duration {
        Duration::from_secs(self.expires_at.saturating_sub(epoch_seconds())).saturating_sub(margin)
    }
}
#[derive(Deserialize)]
struct TokenPair {
    access_token: Zeroizing<String>,
    refresh_token: Zeroizing<String>,
    user: Account,
}
#[derive(Deserialize)]
struct AccessToken {
    access_token: Zeroizing<String>,
}
#[derive(Deserialize)]
struct TimingClaims {
    sub: String,
    exp: u64,
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn timing_claims(token: &str, expected_id: &str) -> Result<TimingClaims> {
    let mut parts = token.split('.');
    let header = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(Error::LegacyIdentity)?;
    let payload = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or(Error::LegacyIdentity)?;
    if parts.next().is_none_or(str::is_empty) || parts.next().is_some() || header.is_empty() {
        return Err(Error::LegacyIdentity);
    }
    let bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Error::Protocol)?,
    );
    // This unverified decode only schedules refresh and rejects suspicious IDs.
    // It never grants authority: the gateway verifies the JWT at connect.
    let claims: TimingClaims = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
    if matches!(claims.sub.as_str(), "system" | "admin_fallback") {
        return Err(Error::LegacyIdentity);
    }
    if claims.sub != expected_id {
        return Err(Error::IdentityChanged);
    }
    if claims.exp <= epoch_seconds() {
        return Err(Error::LoginRequired);
    }
    Ok(claims)
}
impl TokenPair {
    fn into_session(self) -> Result<Session> {
        self.user.validate()?;
        let claims = timing_claims(&self.access_token, &self.user.id)?;
        timing_claims(&self.refresh_token, &self.user.id)?;
        Ok(Session {
            account: self.user,
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            expires_at: claims.exp,
        })
    }
}

struct HttpReply {
    status: u16,
    body: Zeroizing<Vec<u8>>,
}
#[async_trait]
trait AuthHttp: Send + Sync {
    async fn request(
        &self,
        path: &'static str,
        body: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<HttpReply>;
}
struct RestHttp {
    target: GatewayUrl,
    client: reqwest::Client,
}
impl RestHttp {
    fn build_request(
        &self,
        path: &'static str,
        body: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<reqwest::RequestBuilder> {
        let url = self.target.endpoint(path);
        let request = match body {
            Some(body) => self
                .client
                .post(url)
                .header("content-type", "application/json")
                // HTTP/TLS libraries own wire copies which cannot be zeroized by us.
                .body(body.to_vec()),
            None => self.client.get(url),
        };
        if path == "/api/session/local" {
            if !self.target.is_loopback() {
                return Err(Error::LocalOnly);
            }
            // Required by local_session::evaluate, even for native clients.
            Ok(request.header("x-duduclaw-local", "1"))
        } else {
            Ok(request)
        }
    }
}
#[async_trait]
impl AuthHttp for RestHttp {
    async fn request(
        &self,
        path: &'static str,
        body: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<HttpReply> {
        let request = self.build_request(path, body)?;
        let mut response = request.send().await.map_err(|_| Error::Network)?;
        let status = response.status().as_u16();
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
            if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
                return Err(Error::Protocol);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(HttpReply {
            status,
            body: bytes,
        })
    }
}

#[derive(Clone)]
pub struct AuthClient {
    target: GatewayUrl,
    http: Arc<dyn AuthHttp>,
}
impl AuthClient {
    pub fn new(target: GatewayUrl) -> Result<Self> {
        let client = target.http_client()?;
        Ok(Self {
            http: Arc::new(RestHttp {
                target: target.clone(),
                client,
            }),
            target,
        })
    }
    pub fn target(&self) -> &GatewayUrl {
        &self.target
    }
    async fn post<T: Serialize + Sync>(&self, path: &'static str, body: &T) -> Result<HttpReply> {
        let body = Zeroizing::new(serde_json::to_vec(body).map_err(|_| Error::Protocol)?);
        self.http.request(path, Some(body)).await
    }
    fn decode<T: DeserializeOwned>(reply: HttpReply) -> Result<T> {
        if !(200..300).contains(&reply.status) {
            return Err(Error::LoginRequired);
        }
        serde_json::from_slice(&reply.body).map_err(|_| Error::Protocol)
    }
    /// An explicit local attempt can never be redirected or sent off-loopback.
    pub async fn local_session(&self) -> Result<Session> {
        if !self.target.is_loopback() {
            return Err(Error::LocalOnly);
        }
        Self::decode::<TokenPair>(self.post("/api/session/local", &()).await?)?.into_session()
    }
    pub async fn login(&self, mode: LoginMode, prompt: &mut dyn LoginPrompt) -> Result<Session> {
        if matches!(mode, LoginMode::Personal) && self.target.is_loopback() {
            let reply = self.post("/api/session/local", &()).await?;
            if (200..300).contains(&reply.status) {
                return Self::decode::<TokenPair>(reply)?.into_session();
            }
            // Only the server's uniform local-session refusal enables password
            // login; network/5xx/redirect errors never silently weaken login.
            if reply.status != 403 {
                return Err(Error::LoginRequired);
            }
            #[derive(Deserialize)]
            struct FirstRun {
                claimable: bool,
            }
            let first: FirstRun =
                Self::decode(self.http.request("/api/first-run/status", None).await?)?;
            if first.claimable {
                return Err(Error::SetupRequired);
            }
        }
        let email = prompt.read(PromptField::Email).await?;
        let password = prompt.read(PromptField::Password).await?;
        #[derive(Serialize)]
        struct PasswordBody<'a> {
            email: &'a str,
            password: &'a str,
        }
        let pair: TokenPair = Self::decode(
            self.post(
                "/api/login",
                &PasswordBody {
                    email: &email,
                    password: &password,
                },
            )
            .await?,
        )?;
        drop(password);
        let session = pair.into_session()?;
        if matches!(mode, LoginMode::Personal) {
            return Ok(session);
        }
        let expected_id = session.account.id.clone();
        // Current gateway OTP endpoints are standalone passwordless login,
        // not server-bound MFA. Require both locally, discard password tokens,
        // and require OTP to resolve to the same account.
        drop(session);
        #[derive(Serialize)]
        struct OtpRequest<'a> {
            email: &'a str,
        }
        #[derive(Deserialize)]
        struct Challenge {
            challenge_id: Zeroizing<String>,
            sent: bool,
        }
        let challenge: Challenge = Self::decode(
            self.post("/api/otp/request", &OtpRequest { email: &email })
                .await?,
        )?;
        if !challenge.sent || challenge.challenge_id.is_empty() {
            return Err(Error::Protocol);
        }
        let code = prompt.read(PromptField::Otp).await?;
        #[derive(Serialize)]
        struct OtpVerify<'a> {
            challenge_id: &'a str,
            code: &'a str,
        }
        let pair: TokenPair = Self::decode(
            self.post(
                "/api/otp/verify",
                &OtpVerify {
                    challenge_id: &challenge.challenge_id,
                    code: &code,
                },
            )
            .await?,
        )?;
        let session = pair.into_session()?;
        if session.account.id != expected_id {
            return Err(Error::IdentityChanged);
        }
        Ok(session)
    }
    pub async fn refresh(&self, session: &mut Session) -> Result<()> {
        #[derive(Serialize)]
        struct Refresh<'a> {
            refresh_token: &'a str,
        }
        let refreshed = async {
            let reply = self
                .post(
                    "/api/refresh",
                    &Refresh {
                        refresh_token: &session.refresh_token,
                    },
                )
                .await?;
            let access: AccessToken = Self::decode(reply)?;
            let claims = timing_claims(&access.access_token, &session.account.id)?;
            Ok::<_, Error>((access, claims.exp))
        }
        .await;
        match refreshed {
            Ok((access, expires_at)) => {
                session.access_token = access.access_token;
                session.expires_at = expires_at;
                // /api/refresh returns only access_token. Retain the refresh token.
                Ok(())
            }
            Err(_) => {
                // Even a caller outside the socket actor cannot reuse a failed
                // refresh session. Login must acquire a new credential pair.
                session.access_token.zeroize();
                session.refresh_token.zeroize();
                session.expires_at = 0;
                Err(Error::LoginRequired)
            }
        }
    }
}

/// Call before prompting or acquiring credentials, at the phase-3 CLI entry.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn disable_core_dumps() -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: setrlimit reads a valid stack-allocated rlimit; no pointers escape.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
        return Err(Error::CoreDumpProtection);
    }
    Ok(())
}
#[cfg(not(unix))]
pub fn disable_core_dumps() -> Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
