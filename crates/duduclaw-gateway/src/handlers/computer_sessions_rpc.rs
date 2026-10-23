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
    pub(crate) async fn handle_computer_profiles(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        use crate::computer_use_sessions::live_view::LiveAction;
        let agent = params["agent_id"].as_str().unwrap_or("");
        if !duduclaw_core::is_valid_agent_id(agent) {
            return WsFrame::error_response("", "invalid agent_id");
        }
        let Some(live) = live_identity(&self.home_dir, ctx) else {
            return WsFrame::error_response("", "permission denied");
        };
        let action = if method == "computer_profiles.reset" {
            LiveAction::Control
        } else {
            LiveAction::Watch
        };
        if !authorize(&live, agent, action) {
            return WsFrame::error_response("", "permission denied");
        }
        if method == "computer_profiles.reset" {
            if params["confirm"] != true {
                return WsFrame::error_response("", "清除登入狀態會結束瀏覽器，請明確確認。");
            }
            return match admin_sessions(&self.home_dir)
                .reset_login(agent, &live)
                .await
            {
                Ok(()) => WsFrame::ok_response("", json!({"ok": true})),
                Err(e) => WsFrame::error_response("", &e.message),
            };
        }
        let caps =
            duduclaw_core::agent_toml::load(&self.home_dir.join("agents").join(agent)).capabilities;
        match crate::computer_profiles::state(&self.home_dir, agent) {
            Ok(state) => WsFrame::ok_response(
                "",
                json!({
                    "enabled": crate::computer_profiles::ProfilesConfig::active(&self.home_dir).is_some() && caps.computer_use && caps.computer_use_config.persist_login,
                    "saved": state.is_some(), "sealed_bytes": state.as_ref().map(|s| s.sealed_bytes),
                    "updated_at": state.as_ref().map(|s| s.updated_at),
                    "hosts_count": state.as_ref().map(|s| s.hosts.len()),
                    "can_reset": authorize(&live, agent, LiveAction::Control),
                }),
            ),
            Err(_) => WsFrame::error_response("", "目前無法讀取瀏覽器登入狀態。"),
        }
    }

    pub(crate) async fn handle_computer_sessions(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if method == "computer_sessions.snapshot" {
            return Box::pin(self.handle_computer_snapshot(params, ctx)).await;
        }
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
            return Box::pin(self.handle_computer_team(method, params, ctx)).await;
        }
        let sessions = admin_sessions(&self.home_dir);
        let result = match method {
            "computer_sessions.status" => {
                let mut status = sessions.live_status(agent, &live.user_id);
                let caps =
                    duduclaw_core::agent_toml::load(&self.home_dir.join("agents").join(agent))
                        .capabilities;
                status["can_keep_login"] = json!(
                    authorize(
                        &live,
                        agent,
                        crate::computer_use_sessions::live_view::LiveAction::Control
                    ) && caps.computer_use_config.persist_login
                        && crate::computer_profiles::ProfilesConfig::active(&self.home_dir)
                            .is_some()
                );
                Ok(status)
            }
            "computer_sessions.view" => sessions.live_view_open(agent, &live).await,
            "computer_sessions.takeover" => sessions.live_takeover(agent, &live).await,
            "computer_sessions.hand_back" => {
                sessions
                    .live_hand_back(agent, &live, params["note"].as_str())
                    .await
            }
            "computer_sessions.resume" => sessions.live_resume(agent, &live).await,
            "computer_sessions.stop" => {
                if params.get("keep_login").is_some_and(|v| !v.is_boolean()) {
                    return WsFrame::error_response("", "keep_login must be true or false");
                }
                let expected = match params.get("session_id") {
                    None => None,
                    Some(Value::String(id)) if !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control) => Some(id.as_str()),
                    _ => return WsFrame::error_response("", "invalid session_id"),
                };
                sessions
                    .live_stop_with_login_bound(
                        agent,
                        &live,
                        params["keep_login"].as_bool().unwrap_or(false),
                        expected,
                    )
                    .await
            }
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
