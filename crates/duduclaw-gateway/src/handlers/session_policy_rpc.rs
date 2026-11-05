//! Gateway-owned session administration; never delegated to extensions.
use super::*;

pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "sessions.policy" | "sessions.policy_update" | "users.revoke_sessions" => Some(UserRole::Admin),
        _ => None,
    }
}

impl MethodHandler {
    pub(crate) async fn handle_session_policy_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "無法讀取帳號資料。");
        };
        let method = method.to_owned();
        let actor = ctx.user_id.clone();
        let epoch = ctx.credential_epoch;
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            // The DB rechecks active Admin authority and epoch inside the
            // mutation transaction, and audits both success and refusals.
            match method.as_str() {
                "sessions.policy" => Ok(json!({"days":db.remember_admin_policy(&actor,epoch,None)?})),
                "sessions.policy_update" => {
                    let days = params.get("days").and_then(Value::as_u64)
                        .and_then(|v|u16::try_from(v).ok()).unwrap_or(0);
                    Ok(json!({"days":db.remember_admin_policy(&actor,epoch,Some(days))?}))
                }
                "users.revoke_sessions" => {
                    let target = params.get("user_id").and_then(Value::as_str).unwrap_or("");
                    db.remember_admin_revoke_all(&actor,epoch,target)?;
                    Ok(json!({"revoked":true}))
                }
                _ => Err("不允許此操作。".into()),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("",value),
            Ok(Err(error)) => WsFrame::error_response("",&error),
            Err(_) => WsFrame::error_response("", "無法更新登入設定。"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_admin_registry_is_explicit() {
        for method in ["sessions.policy", "sessions.policy_update", "users.revoke_sessions"] {
            assert_eq!(minimum_role(method), Some(UserRole::Admin));
        }
        assert_eq!(minimum_role("sessions.unknown"), None);
    }
}
