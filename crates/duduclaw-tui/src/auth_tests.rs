use super::*;
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};

fn token(id: &str) -> String {
    format!(
        "e30.{}.sig",
        URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"sub":id,"exp":4_000_000_000u64})).unwrap())
    )
}
fn pair(id: &str, change: bool) -> Value {
    json!({"access_token":token(id),"refresh_token":token(id),"user":{"id":id,"email":"test@example.com","role":"employee","must_change_password":change}})
}
struct Step {
    path: &'static str,
    body: Option<Value>,
    status: u16,
    response: Value,
}
struct Script(Mutex<VecDeque<Step>>);
#[async_trait]
impl AuthHttp for Script {
    async fn request(
        &self,
        path: &'static str,
        body: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<HttpReply> {
        let step = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected REST call");
        assert_eq!(path, step.path);
        let actual = body.map(|b| serde_json::from_slice::<Value>(&b).unwrap());
        // Never include credential-bearing values in assertion diagnostics.
        assert!(actual == step.body, "request shape mismatch");
        Ok(HttpReply {
            status: step.status,
            body: Zeroizing::new(serde_json::to_vec(&step.response).unwrap()),
        })
    }
}
struct Prompt(VecDeque<(PromptField, &'static str)>);
#[async_trait]
impl LoginPrompt for Prompt {
    async fn read(&mut self, field: PromptField) -> Result<Zeroizing<String>> {
        let (expected, value) = self.0.pop_front().expect("unexpected prompt");
        assert_eq!(field, expected);
        Ok(Zeroizing::new(value.to_owned()))
    }
}
fn prompt(otp: bool) -> Prompt {
    let mut p = VecDeque::from([
        (PromptField::Email, "test@example.com"),
        (PromptField::Password, "fixture-password"),
    ]);
    if otp {
        p.push_back((PromptField::Otp, "123456"));
    }
    Prompt(p)
}
fn client(url: &str, steps: Vec<Step>) -> (AuthClient, Arc<Script>) {
    let script = Arc::new(Script(Mutex::new(steps.into())));
    (
        AuthClient {
            target: GatewayUrl::parse(url).unwrap(),
            http: script.clone(),
        },
        script,
    )
}
fn local(status: u16, response: Value) -> Step {
    Step {
        path: "/api/session/local",
        body: Some(Value::Null),
        status,
        response,
    }
}
fn password(response: Value) -> Step {
    Step {
        path: "/api/login",
        body: Some(json!({"email":"test@example.com","password":"fixture-password"})),
        status: 200,
        response,
    }
}
fn first_run(claimable: bool) -> Step {
    Step {
        path: "/api/first-run/status",
        body: None,
        status: 200,
        response: json!({"claimable":claimable}),
    }
}

#[tokio::test]
async fn personal_auto_login_without_prompt() {
    let (c, s) = client("http://localhost", vec![local(200, pair("u1", false))]);
    let session = c
        .login(LoginMode::Personal, &mut Prompt(VecDeque::new()))
        .await
        .unwrap();
    assert_eq!(session.account.role, Role::Employee);
    assert!(session.account.role.admin_panel_notice().is_some());
    assert!(s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn auto_login_off_password_set() {
    let (c, s) = client(
        "http://localhost",
        vec![
            local(403, json!({"error":"local auto-login unavailable"})),
            first_run(false),
            password(pair("u1", false)),
        ],
    );
    assert!(
        c.login(LoginMode::Personal, &mut prompt(false))
            .await
            .is_ok()
    );
    assert!(s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn auto_login_off_no_password() {
    let (c, _) = client(
        "http://localhost",
        vec![local(403, json!({})), first_run(true)],
    );
    assert!(matches!(
        c.login(LoginMode::Personal, &mut Prompt(VecDeque::new()))
            .await,
        Err(Error::SetupRequired)
    ));
}
#[tokio::test]
async fn remote_never_attempts_local_login() {
    let (c, s) = client("https://example.com", vec![password(pair("u1", false))]);
    assert!(matches!(c.local_session().await, Err(Error::LocalOnly)));
    assert!(
        c.login(LoginMode::Personal, &mut prompt(false))
            .await
            .is_ok()
    );
    assert!(s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn team_password_then_otp_real_shapes_and_same_account() {
    for otp_id in ["u1", "u2"] {
        let (c, s) = client(
            "http://localhost",
            vec![
                password(pair("u1", false)),
                Step {
                    path: "/api/otp/request",
                    body: Some(json!({"email":"test@example.com"})),
                    status: 200,
                    response: json!({"challenge_id":"challenge","sent":true}),
                },
                Step {
                    path: "/api/otp/verify",
                    body: Some(json!({"challenge_id":"challenge","code":"123456"})),
                    status: 200,
                    response: pair(otp_id, false),
                },
            ],
        );
        let result = c
            .login(LoginMode::TeamOrEnterprise, &mut prompt(true))
            .await;
        if otp_id == "u1" {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(Error::IdentityChanged)));
        }
        assert!(s.0.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn password_change_and_system_are_refused() {
    for (id, change, expected) in [
        ("u1", true, Error::PasswordChangeRequired),
        ("system", false, Error::LegacyIdentity),
        ("admin_fallback", false, Error::LegacyIdentity),
    ] {
        let (c, _) = client("https://example.com", vec![password(pair(id, change))]);
        assert!(
            matches!(c.login(LoginMode::Personal,&mut prompt(false)).await,Err(e) if e == expected)
        );
    }
}
#[tokio::test]
async fn refresh_keeps_refresh_token_and_failure_requires_login() {
    let (c, s) = client(
        "https://example.com",
        vec![
            password(pair("u1", false)),
            Step {
                path: "/api/refresh",
                body: Some(json!({"refresh_token":token("u1")})),
                status: 200,
                response: json!({"access_token":token("u1")}),
            },
            Step {
                path: "/api/refresh",
                body: Some(json!({"refresh_token":token("u1")})),
                status: 401,
                response: json!({"error":"fixture-secret-never-display"}),
            },
        ],
    );
    let mut session = c
        .login(LoginMode::Personal, &mut prompt(false))
        .await
        .unwrap();
    c.refresh(&mut session).await.unwrap();
    assert_eq!(c.refresh(&mut session).await, Err(Error::LoginRequired));
    assert!(session.access_token.is_empty() && session.refresh_token.is_empty());
    assert_eq!(session.refresh_in(Duration::ZERO), Duration::ZERO);
    assert!(s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn local_errors_do_not_fall_back() {
    for status in [302, 429, 500] {
        let (c, _) = client("http://localhost", vec![local(status, json!({}))]);
        assert!(matches!(
            c.login(LoginMode::Personal, &mut Prompt(VecDeque::new()))
                .await,
            Err(Error::LoginRequired)
        ));
    }
}
#[test]
fn malformed_legacy_and_expired_tokens_are_refused() {
    assert!(matches!(
        timing_claims("admin-token", "u1"),
        Err(Error::LegacyIdentity)
    ));
    let expired = format!(
        "e30.{}.sig",
        URL_SAFE_NO_PAD.encode(br#"{"sub":"u1","exp":1}"#)
    );
    assert!(matches!(
        timing_claims(&expired, "u1"),
        Err(Error::LoginRequired)
    ));
    assert!(matches!(
        timing_claims(&token("u2"), "u1"),
        Err(Error::IdentityChanged)
    ));
    assert!(Role::Admin.admin_panel_notice().is_none());
    assert!(PromptField::Password.secret() && PromptField::Otp.secret());
}

#[test]
fn real_http_local_marker_is_loopback_only() {
    for (url, allowed) in [("http://localhost", true), ("https://example.com", false)] {
        let target = GatewayUrl::parse(url).unwrap();
        let http = RestHttp {
            client: target.http_client().unwrap(),
            target,
        };
        let result =
            http.build_request("/api/session/local", Some(Zeroizing::new(b"null".to_vec())));
        if allowed {
            let request = result.unwrap().build().unwrap();
            assert_eq!(request.headers().get("x-duduclaw-local").unwrap(), "1");
            assert_eq!(request.method(), reqwest::Method::POST);
        } else {
            assert!(matches!(result, Err(Error::LocalOnly)));
        }
        let request = http
            .build_request("/api/login", Some(Zeroizing::new(b"{}".to_vec())))
            .unwrap()
            .build()
            .unwrap();
        assert!(request.headers().get("x-duduclaw-local").is_none());
    }
}
