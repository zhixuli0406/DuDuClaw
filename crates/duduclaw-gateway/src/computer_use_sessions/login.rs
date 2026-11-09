//! Label-only login control. Secret material never enters an action or audit value.
use std::sync::{Arc, atomic::Ordering};
use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;
use super::*;
use crate::website_credentials::{self, Metadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginStatus {
    LoggedIn,
    NeedsTakeover,
    Failed,
    FailedUncleared,
    OriginMismatch,
    FormOriginMismatch,
}
impl LoginStatus {
    pub(crate) fn parse(bytes: &[u8]) -> Self {
        match bytes.strip_suffix(b"\n").unwrap_or(bytes) {
            b"logged_in" => Self::LoggedIn,
            b"needs_takeover" => Self::NeedsTakeover,
            b"failed" => Self::Failed,
            b"origin_mismatch" => Self::OriginMismatch,
            b"form_origin_mismatch" => Self::FormOriginMismatch,
            _ => Self::FailedUncleared,
        }
    }
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::LoggedIn => "logged_in",
            Self::NeedsTakeover => "needs_takeover",
            Self::Failed => "failed",
            Self::FailedUncleared => "failed_uncleared",
            Self::OriginMismatch => "origin_mismatch",
            Self::FormOriginMismatch => "form_origin_mismatch",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Self::LoggedIn => "登入已提交，密碼欄位已消失；請確認網站的登入結果。",
            Self::NeedsTakeover => "網站需要雙重驗證或驗證碼，請請人員使用既有接手功能完成；不要在對話中索取密碼或一次性驗證碼。",
            Self::Failed => "登入未完成，帳號與密碼欄位已清除。",
            Self::FailedUncleared => "無法確認登入欄位已清除，已停止並移除工作階段；請啟動新的工作階段。",
            Self::OriginMismatch => "目前頁面的來源與憑證設定不同，已拒絕填入。",
            Self::FormOriginMismatch => "登入表單的提交來源不符，已拒絕填入密碼。",
        }
    }
    pub(crate) fn response(self) -> Value {
        json!({"ok":true, "status":self.code(), "message":self.message()})
    }
}

/// Also audits request cancellation. No label, username or password is recorded.
struct LoginAudit<'a> {
    home: &'a std::path::Path,
    agent: &'a str,
    credential: Option<Metadata>,
    status: &'static str,
}
impl Drop for LoginAudit<'_> {
    fn drop(&mut self) {
        use duduclaw_security::audit::{AuditEvent, Severity};
        crate::security_autopilot::audit_and_emit(self.home, &AuditEvent::new(
            "website_credential_used", "computer_login", Severity::Info,
            json!({"credential_id":self.credential.as_ref().map(|c| &c.id),
                "agent":self.agent, "origin":self.credential.as_ref().map(|c| &c.origin),
                "status":self.status}),
        ));
    }
}

/// Cancellation leaves the screen masked and stops further actions: a killed
/// docker client does not prove the helper stopped inside the container.
struct LoginFlight {
    shared: Arc<SessionShared>,
    control: Arc<OrchestratorControl>,
    finished: bool,
}
impl Drop for LoginFlight {
    fn drop(&mut self) {
        if !self.finished { self.control.stopped.store(true, Ordering::Release); }
        self.shared.login_running.store(false, Ordering::Release);
    }
}

impl ComputerUseSessions {
    pub(crate) async fn action_with_egress(
        &self, agent: &str, session: Option<&str>, turn: Option<&str>,
        egress_turn: Option<&str>, req: &ActionRequest,
    ) -> Result<Value, OpError> {
        let ActionRequest::Login { credential } = req else {
            return self.action_with_egress_inner(agent, session, turn, egress_turn, req, None).await;
        };
        verify_service_authority()?;
        let metadata = website_credentials::list(&self.home).ok()
            .and_then(|records| records.into_iter().find(|m| m.label == *credential));
        let mut audit = LoginAudit { home: &self.home, agent, credential: metadata, status: "cancelled" };
        let result = async {
            let expected = audit.credential.as_ref().filter(|m| m.allowed_agents.iter().any(|a| a == agent))
                .ok_or_else(login_unavailable)?;
            self.action_with_egress_inner(agent, session, turn, egress_turn, req, Some(expected)).await
        }.await;
        audit.status = match &result {
            Ok(value) => match value["status"].as_str() {
                Some("logged_in") => "logged_in", Some("needs_takeover") => "needs_takeover",
                Some("failed") => "failed", Some("origin_mismatch") => "origin_mismatch",
                Some("form_origin_mismatch") => "form_origin_mismatch", _ => "failed_uncleared",
            },
            Err(e) => e.code.as_str(),
        };
        result
    }

    pub(super) fn login_labels(&self, agent: &str) -> Vec<String> {
        website_credentials::list(&self.home).unwrap_or_default().into_iter()
            .filter(|m| m.allowed_agents.iter().any(|a| a == agent)).map(|m| m.label).collect()
    }

    pub(super) async fn execute_login(
        &self, session: &mut ManagedSession, expected: &Metadata,
        policy: &str, title: &str, screen_hash: &str,
    ) -> Result<LoginStatus, OpError> {
        let shared = Arc::clone(&session.shared);
        let _view = shared.login_view_gate.write().await;
        self.check_alive(session).await?;
        self.ensure_not_paused(session).await?;
        gates::tool_gates(&self.home, &session.agent_id, gates::TOOL_LOGIN).await?;
        self.verify_action_frame(session, title, Some(screen_hash)).await?;
        // Reuse pinned-host, contract, budget, audit and navigation tool gates.
        // A redirect is not authority to fill: the helper checks the exact path.
        self.navigate(session, &expected.login_url, policy).await?;
        self.check_alive(session).await?;
        self.ensure_not_paused(session).await?;
        gates::tool_gates(&self.home, &session.agent_id, gates::TOOL_LOGIN).await?;
        // Re-read both the secret and its grants AFTER all approval waits.
        verify_service_authority()?;
        let record = website_credentials::for_agent(&self.home, &session.agent_id, &expected.label)
            .map_err(|_| login_unavailable())?.ok_or_else(login_unavailable)?;
        if record.metadata != *expected {
            return Err(login_unavailable());
        }
        #[derive(Serialize)]
        struct Input<'a> {
            origin: &'a str, login_path: &'a str, allowed_paths: &'a [String],
            username: &'a str, password: &'a str,
        }
        let input = Zeroizing::new(serde_json::to_vec(&Input {
            origin: &record.metadata.origin, login_path: &record.metadata.login_path,
            allowed_paths: &record.metadata.allowed_paths,
            username: &record.metadata.username, password: &record.password,
        }).map_err(|_| login_unavailable())?);
        drop(record);
        self.final_control_gate(session, policy)?;
        // Live VNC writes take the read side. This drains pre-login bytes and
        // closes every old stream by epoch, including its buffered frames.
        shared.login_epoch.fetch_add(1, Ordering::AcqRel);
        shared.login_running.store(true, Ordering::Release);
        shared.login_masked.store(true, Ordering::Release);
        *shared.snapshot.lock().unwrap_or_else(|p| p.into_inner()) = None;
        let mut flight = LoginFlight { shared: Arc::clone(&shared), control: session.backend.control(), finished: false };
        verify_service_authority()?;
        let status = session.backend.fill_login(&input).await;
        flight.finished = true;
        shared.login_masked.store(status == LoginStatus::FailedUncleared, Ordering::Release);
        if status == LoginStatus::FailedUncleared {
            // Never export fields or permit navigation to release an uncertain mask.
            shared.discard_profile.store(true, Ordering::Release);
            session.backend.control().stopped.store(true, Ordering::Release);
            self.end_and_wait(session, EndReason::Requested).await;
        }
        Ok(status)
    }
}
fn login_unavailable() -> OpError {
    OpError::new(ErrorCode::Forbidden, "此登入憑證不存在、不可讀或未授權給此員工，請管理員在儀表板檢查。")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_is_closed_and_never_echoes_helper_output() {
        for status in [LoginStatus::LoggedIn, LoginStatus::NeedsTakeover, LoginStatus::Failed,
            LoginStatus::FailedUncleared, LoginStatus::OriginMismatch, LoginStatus::FormOriginMismatch] {
            assert_eq!(LoginStatus::parse(status.code().as_bytes()), status);
            assert_eq!(status.response()["status"], status.code());
        }
        for hostile in [b"secret password".as_slice(), b"logged_in\nsecret", b"", b"logged_in\r\n"] {
            assert_eq!(LoginStatus::parse(hostile), LoginStatus::FailedUncleared);
        }
    }
    #[test]
    fn action_tracing_does_not_record_typed_secrets() {
        #[derive(Clone, Default)]
        struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt().with_ansi(false)
            .with_writer(move || writer.clone()).finish();
        let action = ComputerAction::Type { text: "SECRET-LOG-CANARY".into() };
        let request = ActionRequest::Type { text: "SECRET-LOG-CANARY".into() };
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(?action, ?request, "action failed");
        });
        let output = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains("action failed"));
        assert!(!output.contains("SECRET-LOG-CANARY"));
    }
    #[test]
    fn login_has_its_own_gate_and_debug_never_has_input() {
        let request = ActionRequest::Login { credential: "store-account".into() };
        assert_eq!(action_tool(&request), gates::TOOL_LOGIN);
        assert_ne!(gates::TOOL_LOGIN, gates::TOOL_TYPE);
        let marker = "NEVER-LOG-THIS-PASSWORD";
        assert!(!format!("{:?}", ActionRequest::Type { text: marker.into() }).contains(marker));
        assert!(!format!("{:?}", ComputerAction::Type { text: marker.into() }).contains(marker));
        assert!(!format!("{request:?}").contains("store-account"));
    }
}
