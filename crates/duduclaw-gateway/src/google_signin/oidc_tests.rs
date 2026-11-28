//! Local RSA/JWKS fixtures; no Google accounts or process-wide endpoint changes.
use super::oidc::*;
use crate::google_personal::oauth;
use axum::{Form, Json, Router, routing::{get, post}};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::{RsaPrivateKey, RsaPublicKey, pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding}, traits::PublicKeyParts};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::{OnceLock, Arc, atomic::{AtomicUsize, Ordering}}};

const NOW: i64 = 1_800_000_000;
const CLIENT: &str = "signin-client";
const NONCE: &str = "test-nonce";

struct SigningFixture {
    private_pem: String,
    public_pem: String,
    jwks: Value,
}

fn keys() -> &'static SigningFixture {
    static KEYS: OnceLock<SigningFixture> = OnceLock::new();
    KEYS.get_or_init(|| {
        let private = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let public = RsaPublicKey::from(&private);
        SigningFixture {
            private_pem: private.to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
            public_pem: public.to_public_key_pem(LineEnding::LF).unwrap(),
            jwks: json!({"keys": [{"kty": "RSA", "alg": "RS256", "use": "sig",
                "kid": "test-key", "n": URL_SAFE_NO_PAD.encode(public.n().to_bytes_be()),
                "e": URL_SAFE_NO_PAD.encode(public.e().to_bytes_be())}]}),
        }
    })
}

struct Provider {
    base: String,
    task: tokio::task::JoinHandle<()>,
    fetches: Arc<AtomicUsize>,
}

impl Drop for Provider {
    fn drop(&mut self) { self.task.abort(); }
}

impl Provider {
    async fn start() -> Self {
        let jwks = keys().jwks.clone();
        let fetches = Arc::new(AtomicUsize::new(0));
        let counter = fetches.clone();
        let app = Router::new()
            .route("/www.googleapis.com/oauth2/v3/certs", get(move || {
                let doc = jwks.clone();
                counter.fetch_add(1, Ordering::SeqCst);
                async move { Json(doc) }
            }))
            .route("/oauth2.googleapis.com/token", post(token_endpoint));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        Self { base, task, fetches }
    }

    async fn verify(&self, token: &str, domains: &[String]) -> Result<VerifiedIdToken, IdTokenError> {
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let expected: [u8; 32] = Sha256::digest(NONCE.as_bytes()).into();
        oauth::TEST_ENDPOINT_BASE.scope(self.base.clone(), async {
            verify_google_id_token(&http, token, CLIENT, &expected, domains, NOW).await
        }).await
    }
}

async fn token_endpoint(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
    for (key, expected) in [("grant_type", "authorization_code"), ("client_id", CLIENT),
        ("client_secret", "test-secret"), ("code_verifier", "test-verifier"),
        ("redirect_uri", "https://app.example/callback")] {
        assert_eq!(form.get(key).map(String::as_str), Some(expected));
    }
    let id = match form.get("code").map(String::as_str) {
        Some("valid") => json!("header.payload.signature"),
        Some("empty") => json!(""),
        Some("two-parts") => json!("header.payload"),
        Some("four-parts") => json!("a.b.c.d"),
        Some("empty-part") => json!("a..c"),
        Some("oversize") => json!(format!("a.b.{}", "c".repeat(16 * 1024))),
        Some("wrong-type") => json!(123),
        _ => return Json(json!({"access_token": "discard-access", "refresh_token": "discard-refresh"})),
    };
    // Other token fields deliberately do not satisfy parse_tokens().
    Json(json!({"id_token": id, "access_token": "discard-access", "refresh_token": "discard-refresh"}))
}

fn claims() -> Value {
    json!({"iss": "https://accounts.google.com", "aud": CLIENT, "azp": CLIENT,
        "sub": "123456789", "email": "person@example.com", "email_verified": true,
        "hd": "example.com", "name": "Test Person", "exp": NOW + 3600,
        "iat": NOW, "nonce": NONCE, "auth_time": NOW - 10})
}

fn signed(value: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    encode(&header, value, &EncodingKey::from_rsa_pem(keys().private_pem.as_bytes()).unwrap()).unwrap()
}

fn allowed() -> Vec<String> { vec!["example.com".into()] }

#[tokio::test]
async fn valid_token_and_both_issuers_pass() {
    let provider = Provider::start().await;
    for issuer in ["accounts.google.com", "https://accounts.google.com"] {
        let mut value = claims();
        value["iss"] = json!(issuer);
        let verified = provider.verify(&signed(&value), &allowed()).await.unwrap();
        assert_eq!(verified.sub, "123456789");
        assert_eq!(verified.email, "person@example.com");
        assert_eq!(verified.hd, "example.com");
        assert_eq!(verified.name, "Test Person");
        assert_eq!(verified.auth_time, Some(NOW - 10));
    }
    assert_eq!(provider.fetches.load(Ordering::SeqCst), 1);
}

macro_rules! invalid_claim_test {
    ($name:ident, $claim:literal, $value:expr) => {
        #[tokio::test]
        async fn $name() {
            let provider = Provider::start().await;
            let mut value = claims();
            value[$claim] = json!($value);
            assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::Invalid)));
            // Identity classification must not mask an authentication failure.
            value.as_object_mut().unwrap().remove("hd");
            assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::Invalid)));
        }
    };
}

invalid_claim_test!(wrong_audience, "aud", "another-client");
invalid_claim_test!(audience_array_rejected, "aud", [CLIENT]);
invalid_claim_test!(wrong_issuer, "iss", "https://accounts.google.com.evil.example");
invalid_claim_test!(azp_mismatch, "azp", "another-client");
invalid_claim_test!(azp_null_rejected, "azp", Value::Null);
invalid_claim_test!(expired_beyond_leeway, "exp", NOW - 61);
invalid_claim_test!(future_iat, "iat", NOW + 61);
invalid_claim_test!(non_integer_iat, "iat", 1800000000.5);
invalid_claim_test!(nonce_mismatch, "nonce", "other-nonce");
invalid_claim_test!(bad_sub, "sub", "12a34");
invalid_claim_test!(email_wrong_type, "email", 123);
invalid_claim_test!(email_bad_shape, "email", "not-an-email");
invalid_claim_test!(malformed_auth_time, "auth_time", "1800000000");

#[tokio::test]
async fn missing_required_claims_are_invalid() {
    let provider = Provider::start().await;
    for key in ["iss", "aud", "exp", "iat", "nonce", "sub", "email"] {
        let mut value = claims();
        value.as_object_mut().unwrap().remove(key);
        value.as_object_mut().unwrap().remove("hd");
        assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::Invalid)), "{key}");
    }
}

#[tokio::test]
async fn tampered_signature_and_disallowed_algorithms() {
    let provider = Provider::start().await;
    let token = signed(&claims());
    let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
    let mut signature = URL_SAFE_NO_PAD.decode(&parts[2]).unwrap();
    signature[0] ^= 1;
    parts[2] = URL_SAFE_NO_PAD.encode(signature);
    assert!(matches!(provider.verify(&parts.join("."), &allowed()).await, Err(IdTokenError::Invalid)));
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("test-key".into());
    let hs = encode(&header, &claims(), &EncodingKey::from_secret(keys().public_pem.as_bytes())).unwrap();
    let none = format!("{}.{}.", URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"test-key"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims()).unwrap()));
    let mut other = Header::new(Algorithm::RS384);
    other.kid = Some("test-key".into());
    let other = encode(&other, &claims(), &EncodingKey::from_rsa_pem(keys().private_pem.as_bytes()).unwrap()).unwrap();
    for token in [hs, none, other, "malformed".into()] {
        assert!(matches!(provider.verify(&token, &allowed()).await, Err(IdTokenError::Invalid)));
    }
    let no_kid = encode(&Header::new(Algorithm::RS256), &claims(),
        &EncodingKey::from_rsa_pem(keys().private_pem.as_bytes()).unwrap()).unwrap();
    assert!(matches!(provider.verify(&no_kid, &allowed()).await, Err(IdTokenError::Invalid)));
}

#[tokio::test]
async fn missing_hd_is_personal() {
    let provider = Provider::start().await;
    let mut value = claims();
    value.as_object_mut().unwrap().remove("hd");
    assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::PersonalAccount)));
}

#[tokio::test]
async fn hd_from_hint_or_email_rejected() {
    let provider = Provider::start().await;
    let mut value = claims();
    value.as_object_mut().unwrap().remove("hd");
    value["email"] = json!("person@example.com");
    value["login_hint"] = json!("person@example.com");
    assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::PersonalAccount)));
}

#[tokio::test]
async fn subdomain_and_alias_not_matched() {
    let provider = Provider::start().await;
    for hd in ["other.com", "sub.example.com", "example.co"] {
        let mut value = claims();
        value["hd"] = json!(hd);
        assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::DomainNotAllowed)));
    }
}

#[tokio::test]
async fn domain_and_verified_email_fail_closed() {
    let provider = Provider::start().await;
    for verified in [json!(false), json!("true"), Value::Null, json!(1)] {
        let mut value = claims();
        value["email_verified"] = verified;
        assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::DomainNotAllowed)));
    }
    let mut value = claims();
    value.as_object_mut().unwrap().remove("email_verified");
    assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::DomainNotAllowed)));
    assert!(matches!(provider.verify(&signed(&claims()), &[]).await, Err(IdTokenError::DomainNotAllowed)));
    for hd in [json!(null), json!(42), json!(""), json!("example.com "), json!("例子.com"), json!("*.example.com")] {
        let mut value = claims();
        value["hd"] = hd;
        assert!(matches!(provider.verify(&signed(&value), &allowed()).await, Err(IdTokenError::DomainNotAllowed)));
    }
}

#[tokio::test]
async fn uppercase_hd_optional_claims_and_clock_boundaries() {
    let provider = Provider::start().await;
    let mut value = claims();
    value["hd"] = json!("EXAMPLE.COM");
    value["exp"] = json!(NOW - 60);
    value["iat"] = json!(NOW + 60);
    for key in ["azp", "name", "auth_time"] { value.as_object_mut().unwrap().remove(key); }
    let token = provider.verify(&signed(&value), &allowed()).await.unwrap();
    assert_eq!(token.hd, "example.com");
    assert_eq!(token.name, "");
    assert_eq!(token.auth_time, None);
}

#[tokio::test]
async fn shared_jwks_cache_and_forced_refetch_floor() {
    let provider = Provider::start().await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let jwks_url = format!("{}/www.googleapis.com/oauth2/v3/certs", provider.base);
    let mut value = claims();
    value["exp"] = json!(chrono::Utc::now().timestamp() + 3600);
    let token = signed(&value);
    crate::webhook_jwt::verify_rs256(&http, &token, &jwks_url, "https://accounts.google.com", CLIENT).await.unwrap();
    crate::webhook_jwt::verify_rs256_any_issuer(&http, &token, &jwks_url,
        &["accounts.google.com", "https://accounts.google.com"], CLIENT, 60).await.unwrap();
    assert_eq!(provider.fetches.load(Ordering::SeqCst), 1);
    for kid in ["unknown-one", "unknown-two"] {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.into());
        let token = encode(&header, &value, &EncodingKey::from_rsa_pem(keys().private_pem.as_bytes()).unwrap()).unwrap();
        assert!(crate::webhook_jwt::verify_rs256_any_issuer(&http, &token, &jwks_url,
            &["https://accounts.google.com"], CLIENT, 60).await.is_err());
    }
    assert_eq!(provider.fetches.load(Ordering::SeqCst), 2);
    // The legacy webhook verifier retains its 300-second expiration leeway.
    value["exp"] = json!(chrono::Utc::now().timestamp() - 120);
    let token = signed(&value);
    assert!(crate::webhook_jwt::verify_rs256(&http, &token, &jwks_url, "https://accounts.google.com", CLIENT).await.is_ok());
    assert!(crate::webhook_jwt::verify_rs256_any_issuer(&http, &token, &jwks_url,
        &["https://accounts.google.com"], CLIENT, 60).await.is_err());
    assert!(crate::webhook_jwt::verify_rs256_any_issuer(&http, &token, &jwks_url, &[], CLIENT, 300).await.is_err());
}

#[tokio::test]
async fn authorization_url_encodes_oidc_and_reauth_parameters() {
    oauth::TEST_ENDPOINT_BASE.scope("http://127.0.0.1:12345".into(), async {
        for (domains, hd) in [(allowed(), "example.com"), (vec!["example.com".into(), "other.com".into()], "*"), (vec![], "*")] {
            for purpose in [SigninPurpose::Login, SigninPurpose::Reauth] {
                let raw = authorization_url("client & id", "https://app.example/cb?x=1&y=2", "state +&", "nonce /+", "challenge_-1",
                    &domains, purpose, Some("person+tag@example.com"));
                let url = url::Url::parse(&raw).unwrap();
                assert_eq!(url.path(), "/accounts.google.com/o/oauth2/v2/auth");
                let pairs: HashMap<_, _> = url.query_pairs().into_owned().collect();
                for (key, value) in [("client_id", "client & id"), ("redirect_uri", "https://app.example/cb?x=1&y=2"),
                    ("response_type", "code"), ("scope", "openid email profile"), ("state", "state +&"),
                    ("nonce", "nonce /+"), ("code_challenge", "challenge_-1"), ("code_challenge_method", "S256"),
                    ("access_type", "online"), ("prompt", "select_account"), ("hd", hd), ("login_hint", "person+tag@example.com")] {
                    assert_eq!(pairs.get(key).map(String::as_str), Some(value));
                }
                assert!(!raw.contains("prompt=login"));
                if purpose == SigninPurpose::Reauth {
                    assert_eq!(pairs.get("max_age").map(String::as_str), Some("0"));
                    assert_eq!(serde_json::from_str::<Value>(pairs.get("claims").unwrap()).unwrap(),
                        json!({"id_token":{"auth_time":{"essential":true}}}));
                } else {
                    assert!(!pairs.contains_key("claims"));
                    assert!(!pairs.contains_key("max_age"));
                }
            }
        }
        let raw = authorization_url(CLIENT, "https://app.example/cb", "s", "n", "c", &allowed(), SigninPurpose::Login, None);
        assert!(!url::Url::parse(&raw).unwrap().query_pairs().any(|(key, _)| key == "login_hint"));
    }).await;
}

#[test]
fn random_values_and_s256() {
    let state = random_state().unwrap();
    let nonce = random_nonce().unwrap();
    let verifier = random_pkce_verifier().unwrap();
    for value in [&state, &nonce, &verifier] {
        assert_eq!(value.len(), 43);
        assert_eq!(URL_SAFE_NO_PAD.decode(value).unwrap().len(), 32);
        assert!(value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')));
    }
    assert_ne!(state, nonce);
    assert_ne!(nonce, verifier);
    assert_eq!(pkce_challenge_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

#[test]
fn reauth_fresh_boundaries() {
    let mut token = VerifiedIdToken { sub: "1".into(), email: "p@example.com".into(),
        hd: "example.com".into(), name: String::new(), auth_time: None };
    assert!(!reauth_fresh(&token, NOW));
    for (at, expected) in [(NOW - 301, false), (NOW - 300, true), (NOW, true), (NOW + 60, true), (NOW + 61, false)] {
        token.auth_time = Some(at);
        assert_eq!(reauth_fresh(&token, NOW), expected);
    }
    token.auth_time = Some(i64::MIN);
    assert!(!reauth_fresh(&token, i64::MAX));
}

#[test]
fn normalize_allowed_hd_cases() {
    for (raw, expected) in [(" EXAMPLE.COM ", "example.com"), ("sub.example.com", "sub.example.com"),
        ("a-b.example", "a-b.example"), ("xn--bcher-kva.example", "xn--bcher-kva.example")] {
        assert_eq!(normalize_allowed_hd(raw).as_deref(), Some(expected));
    }
    for raw in ["", " ", "例子.com", "bücher.example", "*.example.com", ".example.com", "example.com.",
        "-example.com", "example.com-", "a..example.com", "a_b.example", "a/b.example", "a b.example", "a\u{0000}.example"] {
        assert_eq!(normalize_allowed_hd(raw), None);
    }
    assert!(normalize_allowed_hd(&"a".repeat(253)).is_some());
    assert!(normalize_allowed_hd(&"a".repeat(254)).is_none());
}

#[tokio::test]
async fn exchange_for_id_token_only_and_missing_or_malformed_rejected() {
    let provider = Provider::start().await;
    oauth::TEST_ENDPOINT_BASE.scope(provider.base.clone(), async {
        let result = oauth::exchange_for_id_token(CLIENT, "test-secret", "valid", "test-verifier", "https://app.example/callback").await.unwrap();
        assert_eq!(result, "header.payload.signature");
        for code in ["missing", "empty", "two-parts", "four-parts", "empty-part", "oversize", "wrong-type"] {
            assert_eq!(oauth::exchange_for_id_token(CLIENT, "test-secret", code, "test-verifier", "https://app.example/callback").await,
                Err(oauth::Error::Malformed));
        }
    }).await;
}
