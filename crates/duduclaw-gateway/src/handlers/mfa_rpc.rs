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
        "mfa.overview" | "users.mfa_reset" | "users.mfa_enrollment_code" => Some(UserRole::Admin),
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
            Err(_) if method == "mfa.overview" => false,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let session = if matches!(method, "mfa.enroll_begin" | "mfa.enroll_confirm") {
            match crate::google_personal::session_for(&ctx.user_id) {
                Ok(s) => Some(s), Err(_) => return WsFrame::error_response("", "請重新使用帳號登入後再註冊雙因素驗證。"),
            }
        } else { None };
        if method=="mfa.enroll_begin" && !crate::server::check_login_rate_limit(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), &ctx.email) {
            return WsFrame::error_response("", "嘗試次數過多，請稍後再試。");
        }
        let jwt = self.jwt_config.read().await.clone();
        let previous_epoch = ctx.credential_epoch;
        let home = self.home_dir.clone();
        let id = ctx.user_id.clone();
        let method = method.to_owned();
        let result=tokio::task::spawn_blocking(move || -> Result<Value,String> {
            let user=db.get_user(&id)?.filter(|u|u.status==duduclaw_auth::UserStatus::Active).ok_or("無法讀取帳號資料。")?;
            let now=chrono::Utc::now().timestamp();
            let cipher=crate::mfa::Cipher(&home);
            match method.as_str() {
                "mfa.status"=>Ok(json!({"enabled":db.mfa_active(&id)?,"enforced":required})),
                "mfa.enroll_begin"=> {
                    let password=params.get("password").and_then(Value::as_str).ok_or("請輸入目前密碼。");
                    let token=if let Some(token)=params.get("enrollment_token").and_then(Value::as_str) {
                        // Every begin, including a proof retry, requires fresh password knowledge.
                        db.verify_password(&user.email,password?)?;
                        db.mfa_check_session(token,&id,session.as_deref().unwrap(),now)?;
                        token.to_owned()
                    } else {
                        let token=db.mfa_password_challenge(&id,password?,session.as_deref().unwrap(),now)?;
                        let delivery=db.mfa_prepare_enrollment(&token,required,now)?;
                        if required {
                            // The code travels only through the private blocking result, never the RPC response.
                            return Ok(json!({"enrollment_token":token,"challenge_ref":duduclaw_auth::mfa::enrollment_challenge_ref(&token),"proof_required":true,"_delivery":delivery.map(|o|json!({"channel":o.channel,"target":o.channel_user_id,"code":o.code}))}));
                        }
                        token
                    };
                    db.mfa_prove_enrollment(&token,params.get("proof_code").and_then(Value::as_str).unwrap_or(""),now)?;
                    if required { db.mfa_require_independent_proof(&token,now)?; }
                    let mut payload=crate::mfa::enrollment_payload(db.mfa_begin(&id,Some(&token),&cipher,now)?,&user.email);
                    payload["enrollment_token"]=json!(token);
                    Ok(payload)
                },
                "mfa.enroll_confirm"=> {
                    let code=params.get("code").and_then(Value::as_str).unwrap_or("");
                    let token=params.get("enrollment_token").and_then(Value::as_str).ok_or("缺少註冊憑證。");
                    let token=token?;
                    db.mfa_check_session(token,&id,session.as_deref().unwrap(),now)?;
                    if required { db.mfa_require_independent_proof(token,now)?; }
                    let jwt = jwt.as_ref().ok_or("登入服務無法使用。")?;
                    db.mfa_confirm_and_issue(&id, Some(token), code, &cipher, now, |user, access, codes| {
                        crate::mfa::replacement_session(jwt, user, access, previous_epoch, codes, true)
                    })
                }
                "mfa.disable" | "mfa.recovery_regenerate" => {
                    if method=="mfa.disable" && required { return Err("管理員已要求雙因素驗證，無法停用。".into()); }
                    let code=params.get("code").and_then(Value::as_str).unwrap_or("");
                    if method == "mfa.disable" {
                        let jwt = jwt.as_ref().ok_or("登入服務無法使用。")?;
                        db.mfa_change_and_issue(&id, code, true, &cipher, now, |user, access, codes| {
                            crate::mfa::replacement_session(jwt, user, access, previous_epoch, codes, false)
                        })
                    } else {
                        Ok(json!({"recovery_codes": db.mfa_change(&id, code, false, &cipher, now)?}))
                    }
                }
                "users.mfa_reset"=> {
                    let target=params.get("user_id").and_then(Value::as_str).ok_or("請指定帳號。")?;
                    db.mfa_admin_reset(&id,target,params.get("code").and_then(Value::as_str).unwrap_or(""),&cipher,now)?;
                    Ok(json!({"reset":true}))
                }
                "users.mfa_enrollment_code"=> {
                    let target=params.get("user_id").and_then(Value::as_str).ok_or("請指定帳號。")?;
                    Ok(json!({"enrollment_code":db.mfa_issue_enrollment_code(Some(&id),target,params.get("challenge_ref").and_then(Value::as_str).ok_or("請提供註冊識別碼。")?,now)?,"expires_in":duduclaw_auth::mfa::TTL}))
                }
                "mfa.overview"=> {
                    crate::mfa::require_admin(&db,&id)?;
                    let rows=db.list_users()?.into_iter().map(|u| {
                        Ok(json!({"user_id":u.id,"email":u.email,"enabled":db.mfa_active(&u.id)?,"channel_bindings":db.mfa_channel_binding_times(&u.id)?}))
                    }).collect::<Result<Vec<Value>,String>>()?;
                    Ok(json!({"users":rows,"enforced":crate::mfa::enforced(&home,personal).ok(),"policy":crate::mfa::policy_status(&home)}))
                }
                _=>Err("不允許此操作。".into()),
            }
        }).await;
        match result {
            Ok(Ok(mut value)) => {
                if let Some(delivery)=value.as_object_mut().and_then(|v|v.remove("_delivery")).filter(|v|!v.is_null()) {
                    use crate::otp_delivery::OtpDeliverer;
                    let sender=crate::otp_delivery::ConfigOtpDeliverer::new(self.home_dir.clone(),reqwest::Client::new());
                    if sender.deliver(delivery["channel"].as_str().unwrap_or(""),delivery["target"].as_str().unwrap_or(""),
                        &format!("DuDuClaw 雙因素註冊驗證碼：{}（5 分鐘內有效）",delivery["code"].as_str().unwrap_or(""))).await.is_err() {
                        return WsFrame::error_response("", "註冊驗證碼無法送達，請稍後再試。");
                    }
                }
                WsFrame::ok_response("", value)
            },
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
