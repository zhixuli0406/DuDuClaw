//! Account MFA self-service. Every method is enumerated in dispatch_org.
use super::*;
/// Explicit authorization registry; unknown methods never reach an extension.
pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "mfa.status"
        | "mfa.enroll_begin"
        | "mfa.enroll_confirm"
        | "mfa.recovery_regenerate"
        | "mfa.disable" => Some(UserRole::Employee),
        "mfa.overview" | "users.mfa_reset" => Some(UserRole::Admin),
        _ => None,
    }
}
impl MethodHandler {
    pub(crate) async fn handle_mfa_rpc(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "無法讀取帳號資料。");
        };
        let personal = self.resolve_edition_profile().await.is_personal();
        let required = match crate::mfa::enforced(&self.home_dir, personal) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let home = self.home_dir.clone();
        let id = ctx.user_id.clone();
        let method = method.to_owned();
        let result=tokio::task::spawn_blocking(move || -> Result<Value,String> {
            let user=db.get_user(&id)?.filter(|u|u.status==duduclaw_auth::UserStatus::Active).ok_or("無法讀取帳號資料。")?;
            let now=chrono::Utc::now().timestamp();
            let cipher=crate::mfa::Cipher(&home);
            match method.as_str() {
                "mfa.status"=>Ok(json!({"enabled":db.mfa_active(&id)?,"enforced":required})),
                "mfa.enroll_begin"=>Ok(crate::mfa::enrollment_payload(db.mfa_begin(&id,None,&cipher,now)?,&user.email)),
                "mfa.enroll_confirm"=> {
                    let code=params.get("code").and_then(Value::as_str).unwrap_or("");
                    Ok(json!({"enabled":true,"recovery_codes":db.mfa_confirm(&id,None,code,&cipher,now)?}))
                }
                "mfa.disable" | "mfa.recovery_regenerate" => {
                    if method=="mfa.disable" && required { return Err("管理員已要求雙因素驗證，無法停用。".into()); }
                    let code=params.get("code").and_then(Value::as_str).unwrap_or("");
                    let codes=db.mfa_change(&id,code,method=="mfa.disable",&cipher,now)?;
                    Ok(if method=="mfa.disable" {json!({"enabled":false})} else {json!({"recovery_codes":codes})})
                }
                "users.mfa_reset"=> {
                    let target=params.get("user_id").and_then(Value::as_str).ok_or("請指定帳號。")?;
                    db.mfa_admin_reset(&id,target)?;
                    Ok(json!({"reset":true}))
                }
                "mfa.overview"=> {
                    crate::mfa::require_admin(&db,&id)?;
                    let rows=db.list_users()?.into_iter().map(|u| {
                        Ok(json!({"user_id":u.id,"email":u.email,"enabled":db.mfa_active(&u.id)?}))
                    }).collect::<Result<Vec<Value>,String>>()?;
                    Ok(json!({"users":rows,"enforced":required}))
                }
                _=>Err("不允許此操作。".into()),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(_) => WsFrame::error_response("", "雙因素驗證服務無法使用。"),
        }
    }
    pub(crate) async fn mfa_require_current_admin(&self, ctx: &UserContext) -> Result<(), String> {
        let db = self
            .user_db
            .read()
            .await
            .clone()
            .ok_or("無法讀取帳號資料。")?;
        crate::mfa::require_admin(&db, &ctx.user_id)
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    #[test]
    fn mfa_authorization_registry_is_explicit_and_unknown_denied() {
        for name in [
            "mfa.status",
            "mfa.enroll_begin",
            "mfa.enroll_confirm",
            "mfa.recovery_regenerate",
            "mfa.disable",
        ] {
            assert_eq!(minimum_role(name), Some(UserRole::Employee));
        }
        for name in ["mfa.overview", "users.mfa_reset"] {
            assert_eq!(minimum_role(name), Some(UserRole::Admin));
        }
        for name in [
            "mfa",
            "mfa.unknown",
            "mfa.disable_all",
            "users.mfa_reset_all",
        ] {
            assert_eq!(minimum_role(name), None);
        }
    }
}
