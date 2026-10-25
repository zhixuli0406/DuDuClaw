// Compiled in two private lexical credential owners. No shared bearer decoder.
use crate::google_identity::{
    Refusal,
    effect_authority::{AssertionEffectPermit, ClosedNativeFailure, RemoteRequestBound},
};
use crate::google_identity::home_source::w3_observation::FileProof;
use crate::google_workspace::native_call::NativeCallTemplate;
use duduclaw_llm::{OwnedScopedFailure, OwnedScopedReply};
use serde_json::Value;
use std::path::Path;
use zeroize::Zeroizing;
pub(crate) struct Resolved {
    access: Zeroizing<String>,
    expiry: MaterialExpiry,
    source: FileProof,
    subject: String,
}
#[derive(Clone, Copy)]
struct ExpiryClock {
    wall: u64,
    mono: std::time::Instant,
}
impl ExpiryClock {
    fn now() -> Self {
        Self {
            wall: crate::google_personal::authority::now(),
            mono: std::time::Instant::now(),
        }
    }
}
struct MaterialExpiry {
    wall: u64,
    mono: std::time::Instant,
}
impl MaterialExpiry {
    fn after(now: ExpiryClock, seconds: u64) -> Result<Self, Refusal> {
        if !(121..=86400).contains(&seconds) {
            return Err(unavailable());
        }
        let remaining = seconds - 120;
        Ok(Self {
            wall: now.wall.checked_add(remaining).ok_or_else(unavailable)?,
            mono: now
                .mono
                .checked_add(std::time::Duration::from_secs(remaining))
                .ok_or_else(unavailable)?,
        })
    }
    fn live(&self, clock: ExpiryClock) -> bool {
        clock.wall < self.wall && clock.mono < self.mono
    }
}
fn unavailable() -> Refusal {
    Refusal::soft(
        "google_unreachable",
        "Google 憑證目前無法取得，這次沒有執行資料操作。",
    )
}
fn check_deadline(deadline: tokio::time::Instant) -> Result<(), Refusal> {
    if tokio::time::Instant::now() >= deadline {
        Err(Refusal::hard(
            "selection_expired",
            "Google 帳號準備逾時，請重新操作。",
        ))
    } else {
        Ok(())
    }
}
impl Resolved {
    pub(crate) fn validate(&self) -> Result<(), Refusal> {
        if !self.expiry.live(ExpiryClock::now()) {
            return Err(Refusal::hard(
                "identity_changed",
                "Google 憑證已過期，請重新選擇並核准。",
            ));
        }
        self.source.validate()
    }
    pub(crate) fn live(&self) -> bool {
        self.expiry.live(ExpiryClock::now())
    }
    pub(crate) fn weight(&self) -> usize {
        std::mem::size_of_val(self)
            + self.access.capacity()
            + self.source.weight()
            + self.subject.capacity()
    }
    pub(crate) fn source_digest(&self) -> String {
        self.source.digest()
    }
    pub(crate) fn borrow_for_effect<'a>(
        &'a self,
        permit: AssertionEffectPermit<'a>,
    ) -> Result<Bearer<'a>, Refusal> {
        validate_permit(&permit, self)?;
        Ok(Bearer {
            material: self,
            permit,
        })
    }
}
pub(crate) struct Bearer<'a> {
    material: &'a Resolved,
    permit: AssertionEffectPermit<'a>,
}
mod native_io {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_identity/assertion_native_io.inc.rs"
    ));
}
mod mcp_io {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/google_identity/assertion_mcp_io.inc.rs"
    ));
}
async fn mint(
    key: crate::google_service_account::ServiceAccountKey,
    subject: &str,
    source: FileProof,
    home: &Path,
    deadline: tokio::time::Instant,
    permit: &crate::google_identity::effect_authority::GooglePreflightPermit<'_>,
) -> Result<Resolved, crate::google_identity::effect_authority::CredentialResolveFailure> {
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    source
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    let endpoint = key
        .token_uri
        .as_deref()
        .unwrap_or("https://oauth2.googleapis.com/token");
    let url = reqwest::Url::parse(endpoint).map_err(|_| unavailable())?;
    let valid = url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.port().is_none_or(|p| p == 443);
    #[cfg(not(test))]
    if !valid {
        return Err(Refusal::hard(
            "credential_protection",
            "服務帳號的 token_uri 不符合 HTTPS 安全要求。",
        )
        .into());
    }
    // Even a captured HTTP URI is usable only through the exact owned fixture.
    #[cfg(test)]
    if !valid
        && !(url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.port().is_some_and(|p| p >= 1024)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query().is_none())
    {
        return Err(Refusal::hard(
            "credential_protection",
            "Test issuer URI is not an owned loopback shape.",
        )
        .into());
    }
    #[cfg(test)]
    let expected_endpoint = endpoint.to_owned();
    #[cfg(test)]
    let url = crate::google_identity::effect_authority::variants::testing::issuer(home, endpoint)
        .ok_or_else(|| {
        crate::google_identity::effect_authority::CredentialResolveFailure::Authority(
            Refusal::hard(
                "owned_fixture_missing",
                "Closed issuer owned fixture is missing or changed.",
            ),
        )
    })?;
    #[cfg(not(test))]
    let _ = home;
    let now = ExpiryClock::now();
    #[derive(serde::Serialize)]
    struct Claims<'a> {
        iss: &'a str,
        sub: &'a str,
        scope: String,
        aud: &'a str,
        iat: u64,
        exp: u64,
    }
    let claims = Claims {
        iss: &key.client_email,
        sub: subject,
        scope: crate::google_workspace::REQUIRED_SCOPES.join(" "),
        aud: endpoint,
        iat: now.wall,
        exp: now.wall.checked_add(3600).ok_or_else(unavailable)?,
    };
    let encoding =
        jsonwebtoken::EncodingKey::from_rsa_pem(key.private_key.as_bytes()).map_err(|_| {
            Refusal::hard(
                "credential_protection",
                "服務帳號金鑰內容錯誤，請重新匯入。",
            )
        })?;
    let assertion = Zeroizing::new(
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &encoding,
        )
        .map_err(|_| Refusal::hard("credential_protection", "服務帳號簽署失敗。"))?,
    );
    drop(claims);
    drop(key);
    drop(encoding);
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    source
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .build()
        .map_err(|_| unavailable())?;
    let builder = client.post(url);
    #[cfg(test)]
    let builder = builder.header("x-duduclaw-w3-owned-issuer", "1");
    let request = builder
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", assertion.as_str()),
        ])
        .build()
        .map_err(|_| unavailable())?;
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    source
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    #[cfg(test)]
    if crate::google_identity::effect_authority::variants::testing::issuer(home, &expected_endpoint)
        .is_none()
    {
        return Err(
            crate::google_identity::effect_authority::CredentialResolveFailure::Authority(
                Refusal::hard(
                    "owned_fixture_missing",
                    "Closed issuer owned fixture changed.",
                ),
            ),
        );
    }
    let pending = tokio::time::timeout_at(deadline, client.execute(request)).await;
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    source
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    let mut response = pending
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(Refusal::soft(
            "delegate_rejected",
            "Google 拒絕服務帳號授權，請管理員確認設定。",
        )
        .into());
    }
    if response.content_length().is_some_and(|n| n > 64 * 1024) {
        return Err(unavailable().into());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let next = tokio::time::timeout_at(deadline, response.chunk()).await;
        check_deadline(deadline).map_err(
            crate::google_identity::effect_authority::CredentialResolveFailure::Authority,
        )?;
        permit.validate().map_err(
            crate::google_identity::effect_authority::CredentialResolveFailure::Authority,
        )?;
        source.validate().map_err(
            crate::google_identity::effect_authority::CredentialResolveFailure::Authority,
        )?;
        match next
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?
        {
            Some(chunk) => {
                if bytes
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|n| n > 64 * 1024)
                {
                    return Err(unavailable().into());
                }
                bytes.extend_from_slice(&chunk);
            }
            None => break,
        }
    }
    #[derive(serde::Deserialize)]
    struct Token {
        access_token: String,
        expires_in: u64,
    }
    let token: Token = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if token.access_token.is_empty()
        || token.access_token.len() > 64 * 1024
        || !token
            .access_token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
    {
        return Err(unavailable().into());
    }
    let expiry = MaterialExpiry::after(now, token.expires_in)?;
    let resolved = Resolved {
        access: Zeroizing::new(token.access_token),
        expiry,
        source,
        subject: subject.to_owned(),
    };
    drop(bytes);
    drop(assertion);
    check_deadline(deadline)
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    permit
        .validate()
        .map_err(crate::google_identity::effect_authority::CredentialResolveFailure::Authority)?;
    resolved.validate()?;
    Ok(resolved)
}

#[cfg(test)]
mod w3_expiry_oracles {
    use super::*;
    #[test]
    fn w3_material_private_expiry_uses_both_clocks() {
        let now = ExpiryClock::now();
        let expiry = MaterialExpiry::after(now, 121).unwrap();
        assert!(expiry.live(now));
        assert!(!expiry.live(ExpiryClock {
            wall: now.wall + 2,
            mono: now.mono
        }));
        assert!(!expiry.live(ExpiryClock {
            wall: now.wall,
            mono: now.mono + std::time::Duration::from_secs(2)
        }));
        assert!(MaterialExpiry::after(now, 120).is_err());
    }
}

async fn credential_lease(
    home: &Path,
    deadline: tokio::time::Instant,
    permit: &crate::google_identity::effect_authority::GooglePreflightPermit<'_>,
) -> Result<
    crate::google_personal::grants::Lease,
    crate::google_identity::effect_authority::CredentialResolveFailure,
> {
    use crate::google_identity::effect_authority::CredentialResolveFailure;
    permit
        .validate()
        .map_err(CredentialResolveFailure::Authority)?;
    let pending =
        tokio::time::timeout_at(deadline, crate::google_personal::grants::acquire(home)).await;
    permit
        .validate()
        .map_err(CredentialResolveFailure::Authority)?;
    pending
        .map_err(|_| {
            CredentialResolveFailure::Authority(Refusal::hard(
                "selection_expired",
                "Google 帳號準備逾時。",
            ))
        })?
        .map_err(|_| {
            CredentialResolveFailure::Authority(Refusal::hard(
                "grant_uncertain",
                "Google 授權目前無法確認。",
            ))
        })
}
