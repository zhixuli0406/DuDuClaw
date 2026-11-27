//! Admin-only main-agent list management. P13 does not switch conversation or
//! assignment admission: existing entry points still enforce Admin-only access.

use std::collections::BTreeSet;

use super::*;
use super::dept_grants_rpc::acknowledgement_required;

const RISK_NOTICE: &str = "被加入的人可以和主要 AI 員工對話、交辦工作，也可能問到它記得的內容（包含管理員和它的對話）。建立員工、改其他員工設定、排程仍只限管理員。";
const UNAVAILABLE: &str = "無法讀取主要 AI 員工開放名單，請稍後再試。";
const SAVE_FAILED: &str = "無法儲存主要 AI 員工開放名單，請稍後再試。";
const INVALID_USERS: &str = "請提供成員編號陣列，每個編號都必須是非空白文字，最多 500 筆。";
const MAX_USERS: usize = 500;
const INEFFECTIVE_REASON: &str = "主要 AI 員工目前可以執行命令列指令，名單暫時不會生效。請先在「工具與權限」移除命令列工具，或啟用受保護模式。";

fn list_payload(db: &duduclaw_auth::UserDb) -> Result<Value, &'static str> {
    let ids = db.main_agent_users().map_err(|_| UNAVAILABLE)?;
    let mut users = Vec::with_capacity(ids.len());
    for id in ids {
        let user = db.get_user(&id).map_err(|_| UNAVAILABLE)?.ok_or(UNAVAILABLE)?;
        users.push(json!({
            "user_id": user.id, "display_name": user.display_name, "email": user.email,
            "role": user.role, "status": user.status,
        }));
    }
    Ok(json!({"users": users, "risk_notice": RISK_NOTICE}))
}

async fn with_effectiveness(home: &std::path::Path, mut payload: Value) -> Value {
    use crate::on_behalf_of::access::{listed_access_effective, main_default_targets};
    let nonempty = payload["users"].as_array().is_some_and(|users| !users.is_empty());
    let mut effective = true;
    if nonempty {
        match main_default_targets(home).await {
            Ok(targets) => {
                for target in targets {
                    if !listed_access_effective(home, &target).await.unwrap_or(false) {
                        effective = false;
                        break;
                    }
                }
            }
            Err(_) => effective = false,
        }
    }
    payload["effective"] = json!(effective);
    if nonempty && !effective { payload["ineffective_reason"] = json!(INEFFECTIVE_REASON); }
    payload
}

fn store_message(error: &str) -> &'static str {
    match error {
        duduclaw_auth::dept_grants::ACKNOWLEDGEMENT_REQUIRED => duduclaw_auth::dept_grants::ACKNOWLEDGEMENT_REQUIRED,
        "user not found" => "找不到指定的成員。",
        "main-agent access requires an active user" => "只能加入使用中的成員帳號。",
        _ => SAVE_FAILED,
    }
}

impl MethodHandler {
    // dispatch_org requires a live Admin; gates.rs requires Enterprise.
    pub(crate) async fn handle_main_access_get(&self) -> WsFrame {
        let Some(db) = self.user_db().await else { return WsFrame::error_response("", UNAVAILABLE); };
        match tokio::task::spawn_blocking(move || list_payload(&db)).await {
            Ok(Ok(payload)) => WsFrame::ok_response("", Box::pin(with_effectiveness(&self.home_dir, payload)).await),
            _ => WsFrame::error_response("", UNAVAILABLE),
        }
    }

    pub(crate) async fn handle_main_access_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(raw_ids) = params.get("user_ids").and_then(Value::as_array) else {
            return WsFrame::error_response("", INVALID_USERS);
        };
        // Bound raw input as well as the deduplicated set; never silently drop
        // malformed entries or normalize ids into a different person's account.
        if raw_ids.len() > MAX_USERS { return WsFrame::error_response("", INVALID_USERS); }
        let mut ids = BTreeSet::new();
        for value in raw_ids {
            let Some(id) = value.as_str().filter(|id| !id.trim().is_empty()) else {
                return WsFrame::error_response("", INVALID_USERS);
            };
            ids.insert(id.to_owned());
        }
        let ids: Vec<String> = ids.into_iter().collect();
        let actor = ctx.user_id.clone();
        let acknowledged = params.get("acknowledged").and_then(Value::as_bool) == Some(true);
        let Some(db) = self.user_db().await else { return WsFrame::error_response("", SAVE_FAILED); };
        let result = tokio::task::spawn_blocking(move || {
            db.set_main_agent_users(&actor, &ids, acknowledged).map_err(|error| store_message(&error))?;
            list_payload(&db)
        }).await;
        match result {
            Ok(Ok(payload)) => WsFrame::ok_response("", Box::pin(with_effectiveness(&self.home_dir, payload)).await),
            Ok(Err(duduclaw_auth::dept_grants::ACKNOWLEDGEMENT_REQUIRED)) => acknowledgement_required(),
            Ok(Err(message)) => WsFrame::error_response("", message),
            Err(_) => WsFrame::error_response("", SAVE_FAILED),
        }
    }
}
