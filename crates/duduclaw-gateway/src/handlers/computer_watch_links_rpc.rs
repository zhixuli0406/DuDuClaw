//! Signed browser navigation context is never a viewer grant.
use super::*;
use crate::computer_use_sessions::{
    live_ops::live_identity,
    live_view::{LiveAction, authorize},
    workspace_admin::admin_sessions,
};

fn expired() -> WsFrame {
    WsFrame::error_response("", "這個連結已過期，或目前無法觀看這個瀏覽器。")
}

impl MethodHandler {
    pub(crate) async fn handle_computer_open_link(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(viewer) = live_identity(&self.home_dir, ctx) else { return expired(); };
        if !super::computer_team_rpc::take_view_budget(&self.home_dir, &viewer.user_id) {
            return expired();
        }
        let Some(token) = params["token"].as_str() else { return expired(); };
        let Ok(claims) = crate::computer_watch_links::verify_at(
            &self.home_dir, token, chrono::Utc::now().timestamp(),
        ) else { return expired(); };
        let db = self.user_db.read().await.clone();
        let sessions = admin_sessions(&self.home_dir);
        let visible = || {
            let Some(live) = live_identity(&self.home_dir, ctx) else { return false; };
            authorize(&live, &claims.agent_id, LiveAction::Watch)
                || db.as_ref().is_some_and(|db| {
                    crate::team_oversight::can_view_team_history(&self.home_dir, db, ctx, &claims.agent_id)
                        .is_ok_and(|scope| sessions.team_origin_allows(&claims.agent_id, &claims.session_id, &scope))
                })
        };
        let pinned = || {
            let current = sessions.live_status(&claims.agent_id, &viewer.user_id);
            current["active"] == true
                && current["session_id"].as_str() == Some(claims.session_id.as_str())
                && current["seconds_left"].as_u64().is_some_and(|seconds| seconds > 0)
        };
        if !visible() || !pinned() { return expired(); }
        let Ok(digest) = crate::team_history::audit::session_digest(&self.home_dir, &claims.session_id)
        else { return expired(); };
        if crate::team_history::audit::write(&self.home_dir, "computer_link_opened",
            &viewer.user_id, &claims.agent_id,
            json!({"viewer":viewer.user_id,"agent":claims.agent_id,"session":digest,"source":"navigation_link"}), None,
        ).is_err() { return expired(); }
        // No await after the final identity, live relationship and exact-session
        // fences. A key rotation/expiry while checking the account also refuses.
        if crate::computer_watch_links::verify_at(&self.home_dir, token, chrono::Utc::now().timestamp())
            .is_err() || !visible() || !pinned() { return expired(); }
        WsFrame::ok_response("", json!({"ok":true,"agent_id":claims.agent_id,
            "session_id":claims.session_id,"expires_at":claims.expires_at}))
    }
}
