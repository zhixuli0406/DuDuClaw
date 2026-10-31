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
    async fn method(&mut self) -> Result<LoginMethod> {
        Ok(if self.0.iter().any(|(field, _)| *field == PromptField::Otp) {
            LoginMethod::EmailCode
        } else { LoginMethod::Password })
    }
    async fn mfa_method(&mut self) -> Result<MfaMethod> {
        Ok(if self.0.front().is_some_and(|(field, _)| *field == PromptField::RecoveryCode) {
            MfaMethod::RecoveryCode
        } else { MfaMethod::Authenticator })
    }
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
        p.pop_back();
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
async fn team_email_code_is_a_complete_login_without_password() {
    for otp_id in ["u1", "u2"] {
        let (c, s) = client(
            "http://localhost",
            vec![
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
        assert_eq!(result.unwrap().account.id, otp_id);
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

#[tokio::test]
async fn team_password_is_a_complete_login_without_otp() {
    let (c, s) = client("http://localhost", vec![password(pair("u1", false))]);
    let mut p = prompt(false);
    assert!(c.login(LoginMode::TeamOrEnterprise, &mut p).await.is_ok());
    assert!(s.0.lock().unwrap().is_empty());
    assert!(p.0.is_empty());
}

// The child owns the global log/tracing bridge, so parallel unit tests cannot
// alter the subscriber under test. No socket bind or real credentials involved.
#[test]
fn trace_handshake_never_prints_jwt() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "auth::tests::trace_handshake_child", "--ignored", "--nocapture"])
        .env("RUST_LOG", "trace")
        .output().unwrap();
    let captured = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "handshake child failed");
    assert!(captured.contains("TRACE_CAPTURE_ACTIVE"));
    assert!(!captured.contains(&token("jwt-log-canary")), "JWT leaked");
    assert!(!captured.contains("QUIET_CONTEXT_CANARY"), "TUI tracing was enabled");
    // Also catch byte/hex payload dumps that would not contain the literal JWT.
    for line in captured.lines().filter(|line| line.contains("tungstenite")) {
        assert!(line.contains("TRACE_CAPTURE_ACTIVE"), "network trace escaped suppression");
    }
}

#[tokio::test]
#[ignore = "run only by trace_handshake_never_prints_jwt with captured output"]
async fn trace_handshake_child() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{WebSocketStream, tungstenite::{Message, protocol::Role as WsRole}};
    // A trace-level host is deliberately installed first, including the log
    // facade bridge used by tungstenite. Production CLI installs none.
    tracing_subscriber::fmt().with_env_filter("trace").with_ansi(false).init();
    log::trace!(target: "tungstenite", "TRACE_CAPTURE_ACTIVE");
    let session = serde_json::from_value::<TokenPair>(pair("jwt-log-canary", false)).unwrap().into_session().unwrap();
    crate::without_tracing(async {
        log::trace!(target: "tungstenite", "QUIET_CONTEXT_CANARY");
        let (a, b) = tokio::io::duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(a, WsRole::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(b, WsRole::Server, None).await;
        let server = async {
            let Message::Text(text) = server.next().await.unwrap().unwrap() else { panic!("expected connect") };
            let frame: duduclaw_gateway::protocol::WsFrame = serde_json::from_str(&text).unwrap();
            let duduclaw_gateway::protocol::WsFrame::Request { id, method, params } = frame else { panic!("expected request") };
            assert_eq!(method, "connect");
            assert!(params["jwt"].as_str() == Some(session.access_token()), "token mismatch");
            let response = duduclaw_gateway::protocol::WsFrame::ok_response(&id, json!({"status":"authenticated", "user":{"id":"jwt-log-canary", "email":"test@example.com", "role":"employee"}}));
            server.send(Message::Text(serde_json::to_string(&response).unwrap().into())).await.unwrap();
        };
        let (result, ()) = tokio::join!(crate::client::authenticate(&mut client, &session, 1), server);
        assert!(result.is_ok());
    }).await;
}


#[test]
fn control_room_tokens_never_enter_pty_child_environment() {
    let session = Session {
        account: Account { id: "owner".into(), email: TerminalText::new("owner@example.invalid".into()), role: Role::Admin, must_change_password: false },
        access_token: Zeroizing::new("M4_ACCESS_TOKEN_CANARY".into()),
        refresh_token: Zeroizing::new("M4_REFRESH_TOKEN_CANARY".into()),
        expires_at: u64::MAX,
    };
    let command = crate::pty::command_with_env(std::env::vars_os());
    for (_, value) in command.iter_full_env_as_str() {
        assert!(!value.contains(session.access_token.as_str()));
        assert!(!value.contains(session.refresh_token.as_str()));
    }
}

fn mfa_challenge() -> Value {
    json!({"mfa_required":true,"challenge_id":"mfa-memory-only"})
}
fn mfa_step(field: PromptField, value: &str, status: u16, response: Value) -> Step {
    let key = if field == PromptField::RecoveryCode { "recovery_code" } else { "code" };
    Step {
        path: "/api/mfa/verify",
        body: Some(json!({"challenge_id":"mfa-memory-only", key:value})),
        status,
        response,
    }
}
#[tokio::test]
async fn password_then_authenticator_or_recovery() {
    for (field, value) in [(PromptField::MfaCode, "123456"), (PromptField::RecoveryCode, "fixture-recovery")] {
        let (c, s) = client("https://example.com", vec![
            password(mfa_challenge()), mfa_step(field, value, 200, pair("u1", false)),
        ]);
        let mut p = prompt(false);
        p.0.push_back((field, value));
        assert_eq!(c.login(LoginMode::Personal, &mut p).await.unwrap().account.id, "u1");
        assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
        assert!(field.secret());
    }
}
#[tokio::test]
async fn mfa_wrong_code_then_right() {
    let (c, s) = client("https://example.com", vec![
        password(mfa_challenge()),
        mfa_step(PromptField::MfaCode, "000000", 401, json!({"error":"wrong code"})),
        mfa_step(PromptField::MfaCode, "123456", 200, pair("u1", false)),
    ]);
    let mut p = prompt(false);
    p.0.extend([(PromptField::MfaCode, "000000"), (PromptField::MfaCode, "123456")]);
    assert!(c.login(LoginMode::Personal, &mut p).await.is_ok());
    assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn mfa_server_and_client_attempt_caps() {
    for statuses in [vec![401, 429], vec![401; 5], vec![403], vec![500]] {
        let mut steps = vec![password(mfa_challenge())];
        let mut p = prompt(false);
        for status in statuses {
            steps.push(mfa_step(PromptField::MfaCode, "000000", status, json!({"error":"secret-never-display"})));
            p.0.push_back((PromptField::MfaCode, "000000"));
        }
        let (c, s) = client("https://example.com", steps);
        assert!(matches!(c.login(LoginMode::Personal, &mut p).await, Err(Error::MfaFailed)));
        assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn mfa_enrollment_stops_without_using_token_or_prompting() {
    let (c, s) = client("https://example.com", vec![password(json!({
        "mfa_enrollment_required":true,"enrollment_token":"must-not-be-used"
    }))]);
    let mut p = prompt(false);
    assert!(matches!(c.login(LoginMode::Personal, &mut p).await, Err(Error::MfaEnrollmentRequired)));
    assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
}
struct NonTerminal(Prompt);
#[async_trait]
impl LoginPrompt for NonTerminal {
    fn is_interactive(&self) -> bool { false }
    async fn read(&mut self, field: PromptField) -> Result<Zeroizing<String>> {
        self.0.read(field).await
    }
    async fn mfa_method(&mut self) -> Result<MfaMethod> {
        panic!("non-terminal must not prompt for MFA")
    }
}
#[tokio::test]
async fn mfa_non_terminal_stops_before_code_prompt() {
    let (c, s) = client("https://example.com", vec![password(mfa_challenge())]);
    let mut p = NonTerminal(prompt(false));
    assert!(matches!(c.login(LoginMode::Personal, &mut p).await, Err(Error::MfaTerminalRequired)));
    assert!(s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn channel_code_then_mfa() {
    let (c, s) = client("https://example.com", vec![
        Step { path:"/api/otp/request", body:Some(json!({"email":"test@example.com"})), status:200,
            response:json!({"challenge_id":"channel","sent":true}) },
        Step { path:"/api/otp/verify", body:Some(json!({"challenge_id":"channel","code":"123456"})),
            status:200, response:mfa_challenge() },
        mfa_step(PromptField::MfaCode, "654321", 200, pair("u1", false)),
    ]);
    let mut p = prompt(true);
    p.0.push_back((PromptField::MfaCode, "654321"));
    assert!(c.login(LoginMode::TeamOrEnterprise, &mut p).await.is_ok());
    assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn mfa_invalid_format_is_bounded_without_requests() {
    let (c, s) = client("https://example.com", vec![password(mfa_challenge())]);
    let mut p = prompt(false);
    p.0.extend([(PromptField::MfaCode, "１２３４５６"); 5]);
    assert!(matches!(c.login(LoginMode::Personal, &mut p).await, Err(Error::MfaFailed)));
    assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mfa_can_switch_to_recovery_after_wrong_code() {
    let (c, s) = client("https://example.com", vec![
        password(mfa_challenge()),
        mfa_step(PromptField::MfaCode, "000000", 401, json!({})),
        mfa_step(PromptField::RecoveryCode, "fixture-recovery", 200, pair("u1", false)),
    ]);
    let mut p = prompt(false);
    p.0.extend([(PromptField::MfaCode, "000000"), (PromptField::RecoveryCode, "fixture-recovery")]);
    assert!(c.login(LoginMode::Personal, &mut p).await.is_ok());
    assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn no_mfa_does_not_require_terminal_for_second_step() {
    let (c, s) = client("https://example.com", vec![password(pair("u1", false))]);
    let mut p = NonTerminal(prompt(false));
    assert!(c.login(LoginMode::Personal, &mut p).await.is_ok());
    assert!(s.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn mfa_last_allowed_attempt_can_succeed() {
    let mut steps = vec![password(mfa_challenge())];
    let mut p = prompt(false);
    for _ in 0..4 {
        steps.push(mfa_step(PromptField::MfaCode, "000000", 401, json!({})));
        p.0.push_back((PromptField::MfaCode, "000000"));
    }
    steps.push(mfa_step(PromptField::MfaCode, "123456", 200, pair("u1", false)));
    p.0.push_back((PromptField::MfaCode, "123456"));
    let (c, s) = client("https://example.com", steps);
    assert!(c.login(LoginMode::Personal, &mut p).await.is_ok());
    assert!(p.0.is_empty() && s.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn closed_authentication_socket_is_not_a_network_retry() {
    use futures_util::SinkExt;
    use tokio_tungstenite::{WebSocketStream, tungstenite::{Message, protocol::Role as WsRole}};
    for graceful in [true, false] {
        let session = serde_json::from_value::<TokenPair>(pair("u1", false)).unwrap().into_session().unwrap();
        let (a, b) = tokio::io::duplex(8192);
        let mut client = WebSocketStream::from_raw_socket(a, WsRole::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(b, WsRole::Server, None).await;
        if graceful {
            server.send(Message::Close(None)).await.unwrap();
        }
        drop(server);
        assert!(matches!(crate::client::authenticate(&mut client, &session, 1).await, Err(Error::Disconnected)));
    }
}

#[tokio::test]
async fn rejected_connect_or_auth_close_requires_refresh_but_normal_close_does_not() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{WebSocketStream, tungstenite::{Message, protocol::{Role as WsRole, CloseFrame, frame::coding::CloseCode}}};
    for code in [None, Some(1000), Some(1011), Some(4001), Some(1008)] {
        for rejected in [false, true] {
            let session = serde_json::from_value::<TokenPair>(pair("u1", false)).unwrap().into_session().unwrap();
            let (a, b) = tokio::io::duplex(8192);
            let mut client = WebSocketStream::from_raw_socket(a, WsRole::Client, None).await;
            let mut server = WebSocketStream::from_raw_socket(b, WsRole::Server, None).await;
            let fake = async {
                let _ = server.next().await.unwrap().unwrap();
                let message = if rejected {
                    Message::Text(json!({"type":"res","id":"connect","ok":false,"error":{"code":"unauthorized"}}).to_string().into())
                } else {
                    Message::Close(code.map(|code| CloseFrame { code: CloseCode::from(code), reason: "fixture".into() }))
                };
                server.send(message).await.unwrap();
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(crate::client::authenticate(&mut client, &session, 1), fake)
            }).await.unwrap();
            let expected = if rejected || matches!(code, Some(4001)) {
                Error::LoginRequired
            } else { Error::Disconnected };
            assert!(matches!(result, Err(error) if error == expected), "rejected={rejected}, close={code:?}");
        }
    }
}
