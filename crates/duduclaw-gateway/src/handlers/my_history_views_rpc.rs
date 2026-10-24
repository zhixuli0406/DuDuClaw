//! Metadata-only self service; never an audit-log or audience-query endpoint.
use super::*;
use crate::team_history::{limits, receipts};
use duduclaw_auth::UserStatus;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    before: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    50
}
fn failure(code: &str) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({"code":code,"message":"view records unavailable"})),
    }
}
fn active(db: &UserDb, id: &str) -> Result<(), String> {
    let me = db
        .get_user(id)
        .map_err(|_| "read_error")?
        .ok_or("permission_denied")?;
    if me.status != UserStatus::Active || me.must_change_password {
        return Err("permission_denied".into());
    }
    Ok(())
}
impl MethodHandler {
    pub(crate) async fn handle_my_history_views(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Ok(request) = serde_json::from_value::<Request>(params) else {
            return failure("invalid_params");
        };
        if !(1..=100).contains(&request.limit)
            || request
                .before
                .as_ref()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_err())
        {
            return failure("invalid_params");
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return failure("read_error");
        };
        let home = self.home_dir.clone();
        let user = ctx.user_id.clone();
        static READERS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
            std::sync::OnceLock::new();
        let Ok(permit) = READERS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
            .clone()
            .try_acquire_owned()
        else {
            return failure("busy");
        };
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            active(&db, &user)?;
            // A separate quota bucket from manager list calls. No user can
            // select another bucket or spend another account's quota.
            limits::take(&home, &format!("receipt:{user}"), false).map_err(|_| "rate_limited")?;
            let page = receipts::read(&home, &user, request.before.as_deref(), request.limit)?;
            let mut names = HashMap::new();
            let mut rows = Vec::new();
            for receipt in page.views {
                if !names.contains_key(&receipt.viewer) {
                    let name = db
                        .get_user(&receipt.viewer)
                        .map_err(|_| "read_error")?
                        .map(|u| u.display_name);
                    names.insert(receipt.viewer.clone(), name);
                }
                rows.push((
                    names[&receipt.viewer].clone(),
                    receipt.agent,
                    receipt.timestamp,
                ));
            }
            active(&db, &user)?;
            Ok::<_, String>((rows, page.next_before))
        })
        .await;
        let (rows, next) = match result {
            Ok(Ok(p)) => p,
            Ok(Err(code)) => return failure(&code),
            Err(_) => return failure("read_error"),
        };
        let reg = self.registry.read().await;
        let views:Vec<_>=rows.into_iter().map(|(viewer,agent,timestamp)| {
            let display=reg.get(&agent).map(|a| a.config.agent.display_name.clone());
            json!({"viewer_display_name":viewer,"agent_display_name":display,"timestamp":timestamp,"type":"webchat"})
        }).collect();
        WsFrame::ok_response("", json!({"views":views,"next_before":next}))
    }
}
