//! Reporting-line access never grants a raw stream or browser control.
use super::*;
use crate::computer_use_sessions::live_ops::live_identity;
use crate::computer_use_sessions::live_view::{LiveAction, authorize};
use crate::computer_use_sessions::workspace_admin::admin_sessions;
use crate::team_oversight::{self, TeamOversight};

fn denied() -> WsFrame {
    WsFrame::error_response("", "目前無法觀看這位員工的瀏覽器。")
}

impl MethodHandler {
    pub(crate) async fn handle_computer_snapshot(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let agent = params["agent_id"].as_str().unwrap_or("");
        let Some(live) = live_identity(&self.home_dir, ctx) else {
            return denied();
        };
        if !is_valid_agent_id(agent) {
            return denied();
        }
        if !authorize(&live, agent, LiveAction::Watch) {
            return Box::pin(self.handle_computer_team("computer_sessions.snapshot", params, ctx))
                .await;
        }
        if !take_view_budget(&self.home_dir, &live.user_id) {
            return denied();
        }
        let Some(expected) = params["session_id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128)
        else {
            return denied();
        };
        let sessions = admin_sessions(&self.home_dir);
        let image = match sessions.masked_snapshot(agent, expected).await {
            Ok(image) => image,
            Err(_) => return denied(),
        };
        if !live_identity(&self.home_dir, ctx)
            .is_some_and(|live| authorize(&live, agent, LiveAction::Watch))
        {
            return denied();
        }
        let Ok(digest) = crate::team_history::audit::session_digest(&self.home_dir, expected)
        else {
            return denied();
        };
        if crate::team_history::audit::write(&self.home_dir, "computer_direct_snapshot", &live.user_id, agent,
            json!({"viewer":live.user_id,"agent":agent,"session":digest,"grant_source":"direct","view_mode":"masked"}), Some(format!("{agent}:{expected}"))).is_err() {
            return denied();
        }
        WsFrame::ok_response("", image)
    }

    pub(crate) async fn handle_computer_team(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if !matches!(
            method,
            "computer_sessions.status" | "computer_sessions.snapshot" | "computer_sessions.stop"
        ) {
            return denied();
        }
        let agent = params["agent_id"].as_str().unwrap_or("");
        if !is_valid_agent_id(agent) {
            return denied();
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return denied();
        };
        let scope = match team_oversight::can_view_team_history(&self.home_dir, &db, ctx, agent) {
            Ok(scope) => scope,
            Err(_) => return denied(),
        };
        if method != "computer_sessions.stop"
            && !take_view_budget(&self.home_dir, &scope.viewer_user_id)
        {
            return denied();
        }
        let sessions = admin_sessions(&self.home_dir);
        let current = sessions.live_status(agent, &scope.viewer_user_id);
        let expected = params["session_id"].as_str();
        let id = current["session_id"].as_str();
        let report = id.and_then(|id| sessions.team_report_for(agent, id, &scope));
        let visible = report.is_some();
        if method != "computer_sessions.status"
            && (!visible || expected.is_none() || expected != id)
        {
            return denied();
        }
        let result = match method {
            "computer_sessions.status" => {
                if visible {
                    json!({"ok":true,"active":true,"view_mode":"masked","can_control":false,
                        "session_id":current["session_id"],"state":current["state"],"seconds_left":current["seconds_left"]})
                } else {
                    json!({"ok":true,"active":false,"view_mode":"masked","can_control":false})
                }
            }
            "computer_sessions.snapshot" => {
                match sessions.masked_snapshot(agent, expected.unwrap()).await {
                    Ok(image) => image,
                    Err(_) => {
                        return WsFrame::error_response("", "目前無法提供遮罩畫面，請稍後再試。");
                    }
                }
            }
            _ => {
                if params.get("keep_login").is_some_and(|v| v != false) {
                    return denied();
                }
                if !same_scope(&self.home_dir, &db, ctx, &scope) {
                    return denied();
                }
                if record(
                    &self.home_dir,
                    "computer_team_stopped",
                    &scope,
                    id,
                    report.as_ref(),
                )
                .is_err()
                {
                    return denied();
                }
                return match sessions
                    .live_stop_expected(agent, expected.unwrap(), ctx)
                    .await
                {
                    Ok(result) => WsFrame::ok_response("", result),
                    Err(_) => denied(),
                };
            }
        };
        // No await from this point until the closed disclosure is returned.
        if !same_scope(&self.home_dir, &db, ctx, &scope)
            || (visible && !sessions.team_origin_allows(agent, id.unwrap(), &scope))
        {
            return denied();
        }
        let event = if method == "computer_sessions.snapshot" {
            "computer_team_snapshot"
        } else {
            "computer_team_status"
        };
        if record(
            &self.home_dir,
            event,
            &scope,
            id.filter(|_| visible),
            report.as_ref(),
        )
        .is_err()
        {
            return denied();
        }
        if !same_scope(&self.home_dir, &db, ctx, &scope)
            || (visible && id.and_then(|id| sessions.team_report_for(agent, id, &scope)) != report)
        {
            return denied();
        }
        WsFrame::ok_response("", result)
    }
}

fn same_scope(home: &Path, db: &UserDb, ctx: &UserContext, old: &TeamOversight) -> bool {
    team_oversight::can_view_team_history(home, db, ctx, &old.agent).is_ok_and(|fresh| {
        fresh.since == old.since
            && fresh.generation == old.generation
            && fresh.reports == old.reports
    })
}

fn record(
    home: &Path,
    event: &str,
    scope: &TeamOversight,
    session: Option<&str>,
    report: Option<&(String, u8)>,
) -> Result<(), String> {
    let digest = session
        .map(|id| crate::team_history::audit::session_digest(home, id))
        .transpose()?;
    crate::team_history::audit::write(
        home,
        event,
        &scope.viewer_user_id,
        &scope.agent,
        json!({"viewer":scope.viewer_user_id,"agent":scope.agent,"session":digest,
            "report":report.map(|r| &r.0),"via_depth":report.map(|r|r.1),
            "grant_source":"team","view_mode":"masked","since":scope.since,"generation":scope.generation}),
        Some(format!("{}:{}", scope.agent, session.unwrap_or("inactive"))),
    )
}

/// Polls share a bounded per-person budget across employees. A stop is
/// never throttled by previous viewing; the common image cache separately
/// limits actual captures to one per five seconds for all viewers.
pub(super) fn take_view_budget(home: &Path, viewer: &str) -> bool {
    use std::{
        collections::{HashMap, VecDeque},
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    type Key = (PathBuf, String);
    static BUDGETS: OnceLock<Mutex<HashMap<Key, VecDeque<Instant>>>> = OnceLock::new();
    let Ok(mut budgets) = BUDGETS.get_or_init(Default::default).lock() else {
        return false;
    };
    let now = Instant::now();
    let cutoff = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
    budgets.retain(|_, times| times.back().is_some_and(|last| *last > cutoff));
    let key = (home.to_path_buf(), viewer.to_owned());
    if budgets.len() >= 10_000 && !budgets.contains_key(&key) {
        return false;
    }
    let times = budgets.entry(key).or_default();
    while times.front().is_some_and(|first| *first <= cutoff) {
        times.pop_front();
    }
    if times.len() >= 120 {
        return false;
    }
    times.push_back(now);
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn view_budget_is_shared_across_requests_but_isolated_by_person_and_home() {
        let home = tempfile::tempdir().unwrap();
        for _ in 0..120 {
            assert!(super::take_view_budget(home.path(), "manager-a"));
        }
        assert!(!super::take_view_budget(home.path(), "manager-a"));
        assert!(super::take_view_budget(home.path(), "manager-b"));
        let other = tempfile::tempdir().unwrap();
        assert!(super::take_view_budget(other.path(), "manager-a"));
    }
}
