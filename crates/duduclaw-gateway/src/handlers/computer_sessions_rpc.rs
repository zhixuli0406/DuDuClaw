//! Dashboard RPCs for an employee's live computer-use session (P8):
//! `computer_sessions.{status,view,takeover,hand_back,resume,stop}`.
//!
//! The caller's role and bindings are re-read from `users.db` on every call
//! and judged by `computer_use_sessions::live_view::authorize`: watching
//! (status, view, stop) needs an Admin, a Manager bound to the employee or
//! an account bound at Operator level; taking over, handing back and
//! resuming after an injection pause need an Admin or a Manager bound at
//! Operator level. Answers carry no screenshot or page content; `view` and
//! `takeover` return a one-time viewer ticket and the stream password.
use super::*;
use crate::computer_use_sessions::live_ops::{live_identity, rpc_action};
use crate::computer_use_sessions::live_view::authorize;
use crate::computer_use_sessions::workspace_admin::admin_sessions;

impl MethodHandler {
    pub(crate) async fn handle_computer_sessions(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Some(action) = rpc_action(method) else {
            return WsFrame::error_response("", "unknown computer_sessions method");
        };
        let agent = params["agent_id"].as_str().unwrap_or("");
        if !duduclaw_core::is_valid_agent_id(agent) {
            return WsFrame::error_response("", "invalid agent_id");
        }
        let Some(live) = live_identity(&self.home_dir, ctx) else {
            return WsFrame::error_response("", "permission denied");
        };
        if !authorize(&live, agent, action) {
            return WsFrame::error_response("", "permission denied");
        }
        let sessions = admin_sessions(&self.home_dir);
        let result = match method {
            "computer_sessions.status" => Ok(sessions.live_status(agent, &live.user_id)),
            "computer_sessions.view" => sessions.live_view_open(agent, &live).await,
            "computer_sessions.takeover" => sessions.live_takeover(agent, &live).await,
            "computer_sessions.hand_back" => {
                sessions
                    .live_hand_back(agent, &live, params["note"].as_str())
                    .await
            }
            "computer_sessions.resume" => sessions.live_resume(agent, &live).await,
            "computer_sessions.stop" => sessions.live_stop(agent, &live).await,
            _ => return WsFrame::error_response("", "unknown computer_sessions method"),
        };
        match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e.message),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn failed(f: &WsFrame) -> bool {
        !matches!(f, WsFrame::Response { ok: true, .. })
    }

    #[tokio::test]
    async fn computer_session_rpcs_check_role_and_binding() {
        let dir = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(dir.path().to_path_buf()).await;
        let admin = UserContext::admin_fallback();
        // An Employee-role account with no binding (and no users.db row):
        // denied for every method.
        let mut nobody = admin.clone();
        nobody.user_id = "u-nobody".into();
        nobody.role = UserRole::Employee;
        for method in [
            "computer_sessions.status",
            "computer_sessions.view",
            "computer_sessions.takeover",
            "computer_sessions.hand_back",
            "computer_sessions.resume",
            "computer_sessions.stop",
        ] {
            let f = handler.handle(method, json!({"agent_id": "alice"}), &nobody).await;
            assert!(failed(&f), "{method}");
        }
        // The admin token sees an inactive session, and bad ids are refused.
        let status = handler
            .handle("computer_sessions.status", json!({"agent_id": "alice"}), &admin)
            .await;
        assert!(!failed(&status));
        let text = serde_json::to_string(&status).unwrap();
        assert!(text.contains("\"active\":false"), "{text}");
        let bad = handler
            .handle("computer_sessions.status", json!({"agent_id": "../x"}), &admin)
            .await;
        assert!(failed(&bad));
        let view = handler
            .handle("computer_sessions.view", json!({"agent_id": "alice"}), &admin)
            .await;
        assert!(failed(&view), "no session to watch");
    }
}
