//! Real HTTP contract coverage; NOT RUN where socket binds are refused.
use super::*;
use axum::extract::OriginalUri;
use duduclaw_tui::auth::{LoginMethod, MfaMethod};
use std::collections::VecDeque;

pub(super) struct Step {
    path: &'static str,
    body: Value,
    status: StatusCode,
    response: Value,
}
pub(super) async fn auth_step(
    State(f): State<Arc<Fixture>>,
    OriginalUri(uri): OriginalUri,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let step = f.auth_steps.lock().unwrap().pop_front().expect("unexpected auth request");
    assert_eq!(uri.path(), step.path);
    assert!(body == step.body, "auth request shape mismatch");
    (step.status, Json(step.response))
}
struct Answers {
    fields: VecDeque<(PromptField, &'static str)>,
    interactive: bool,
}
#[async_trait]
impl LoginPrompt for Answers {
    fn is_interactive(&self) -> bool { self.interactive }
    async fn method(&mut self) -> Result<LoginMethod> {
        Ok(if self.fields.iter().any(|(field, _)| *field == PromptField::Otp) {
            LoginMethod::EmailCode
        } else { LoginMethod::Password })
    }
    async fn mfa_method(&mut self) -> Result<MfaMethod> {
        assert!(self.interactive, "non-terminal must never prompt for MFA");
        Ok(if self.fields.front().is_some_and(|(field, _)| *field == PromptField::RecoveryCode) {
            MfaMethod::RecoveryCode
        } else { MfaMethod::Authenticator })
    }
    async fn read(&mut self, field: PromptField) -> Result<Zeroizing<String>> {
        let (expected, value) = self.fields.pop_front().expect("unexpected auth prompt");
        assert_eq!(field, expected);
        Ok(Zeroizing::new(value.into()))
    }
}
fn pair() -> Value {
    json!({"access_token":token(3600,false),"refresh_token":token(7200,false),
        "user":{"id":"u1","email":"fixture@example.com","role":"employee"}})
}
fn challenge() -> Value { json!({"mfa_required":true,"challenge_id":"memory-only"}) }
fn password(response: Value) -> Step {
    Step { path:"/api/login", body:json!({"email":"fixture@example.com","password":"fixture-password"}),
        status:StatusCode::OK, response }
}
fn verify(recovery: bool, status: StatusCode) -> Step {
    let key = if recovery { "recovery_code" } else { "code" };
    Step { path:"/api/mfa/verify", body:json!({"challenge_id":"memory-only",key:if recovery { "fixture-recovery" } else { "123456" }}),
        status, response:if status == StatusCode::OK { pair() } else { json!({"error":"invalid"}) } }
}
#[tokio::test]
async fn password_mfa_recovery_retry_cap_enrollment_non_tty_and_legacy_gateway() {
    for case in ["code", "recovery", "retry", "cap", "enrollment", "non-tty", "legacy"] {
        let Some(gateway) = Gateway::start(false).await else { return; };
        let mut answers = Answers { interactive:case != "non-tty", fields:VecDeque::from([
            (PromptField::Email, "fixture@example.com"), (PromptField::Password, "fixture-password"),
        ]) };
        let mut steps = vec![password(match case {
            "enrollment" => json!({"mfa_enrollment_required":true,"enrollment_token":"unused"}),
            "legacy" => pair(),
            _ => challenge(),
        })];
        if matches!(case, "code" | "recovery" | "retry" | "cap") {
            let recovery = case == "recovery";
            if matches!(case, "retry" | "cap") {
                steps.push(verify(false, StatusCode::UNAUTHORIZED));
                answers.fields.push_back((PromptField::MfaCode, "123456"));
            }
            steps.push(verify(recovery, if case == "cap" { StatusCode::TOO_MANY_REQUESTS } else { StatusCode::OK }));
            answers.fields.push_back(if recovery { (PromptField::RecoveryCode, "fixture-recovery") }
                else { (PromptField::MfaCode, "123456") });
        }
        gateway.fixture.auth_steps.lock().unwrap().extend(steps);
        let result = AuthClient::new(gateway.target.clone()).unwrap()
            .login(LoginMode::TeamOrEnterprise, &mut answers).await;
        match case {
            "cap" => assert!(matches!(result, Err(Error::MfaFailed))),
            "enrollment" => assert!(matches!(result, Err(Error::MfaEnrollmentRequired))),
            "non-tty" => assert!(matches!(result, Err(Error::MfaTerminalRequired))),
            _ => assert_eq!(result.unwrap().account.id, "u1"),
        }
        assert!(answers.fields.is_empty());
        assert!(gateway.fixture.auth_steps.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn channel_primary_login_also_requires_mfa() {
    let Some(gateway) = Gateway::start(false).await else { return; };
    gateway.fixture.auth_steps.lock().unwrap().extend([
        Step { path:"/api/otp/request", body:json!({"email":"fixture@example.com"}), status:StatusCode::OK,
            response:json!({"challenge_id":"channel","sent":true}) },
        Step { path:"/api/otp/verify", body:json!({"challenge_id":"channel","code":"123456"}),
            status:StatusCode::OK, response:challenge() },
        verify(false, StatusCode::OK),
    ]);
    let mut answers = Answers { interactive:true, fields:VecDeque::from([
        (PromptField::Email, "fixture@example.com"), (PromptField::Otp, "123456"), (PromptField::MfaCode, "123456"),
    ]) };
    assert!(AuthClient::new(gateway.target.clone()).unwrap()
        .login(LoginMode::TeamOrEnterprise, &mut answers).await.is_ok());
    assert!(answers.fields.is_empty());
    assert!(gateway.fixture.auth_steps.lock().unwrap().is_empty());
}
