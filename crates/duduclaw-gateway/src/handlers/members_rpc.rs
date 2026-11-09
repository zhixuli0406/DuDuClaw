//! Admin member onboarding. Database creation is atomic per row; SMTP delivery
//! is a separate outcome, so a failed delivery never masquerades as no account.
use super::*;
use duduclaw_auth::members::NewMember;

pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        _ if method.starts_with("users.create_members") || method.starts_with("members.") => Some(UserRole::Admin),
        _ => None,
    }
}

impl MethodHandler {
    pub(crate) async fn handle_users_create_members(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "member_storage_unavailable");
        };
        if !super::user_invites_rpc::live_admin(&db, ctx) {
            return WsFrame::error_response("", "permission denied");
        }
        let Some(rows) = params.get("members").and_then(Value::as_array).filter(|rows| !rows.is_empty() && rows.len() <= 10) else {
            return WsFrame::error_response("", "member_batch_invalid");
        };
        let locale = match params.get("locale") {
            None => "zh-TW",
            Some(Value::String(locale)) if matches!(locale.as_str(), "zh-TW" | "en" | "ja-JP") => locale.as_str(),
            _ => return WsFrame::error_response("", "member_invalid_locale"),
        };
        let mut results = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let member: NewMember = match serde_json::from_value(row.clone()) {
                Ok(member) => member,
                Err(_) => { results.push(json!({"index":index,"error":"member_invalid_input"})); continue; }
            };
            if let Err(code) = member.validate() {
                results.push(json!({"index":index,"error":code})); continue;
            }
            // SR5 round 3 N5: quarantined / tombstoned labels are refused from
            // the lifecycle index (the auth guard reads only the mirror).
            if member.bindings.iter().any(|binding|
                crate::isolation_label_stores::bind_refusal(&self.home_dir, &binding.agent_name).is_some())
            {
                results.push(json!({"index":index,"error":"member_agent_quarantined"})); continue;
            }
            // The public base is checked before creating any invitation-bearing row.
            let base = if member.invite {
                let content_valid = db.invitation_content(locale)
                    .is_ok_and(|content| crate::invitation_email::validate(&content).is_ok());
                if !content_valid {
                    results.push(json!({"index":index,"error":"member_invite_unavailable"})); continue;
                }
                match self.invitation_base(&params).await {
                    Ok(base) => Some(base),
                    Err(_) => { results.push(json!({"index":index,"error":"member_invite_url_required"})); continue; }
                }
            } else { None };
            let registry = self.registry.read().await;
            if member.bindings.iter().any(|binding| registry.get(&binding.agent_name).is_none()) {
                results.push(json!({"index":index,"error":"member_agent_unavailable"})); continue;
            }
            // Invited accounts cannot yet pass the live, password-set identity
            // check required by the existing machine-employee binding route.
            if member.bindings.iter().any(|binding| registry.get(&binding.agent_name)
                .is_some_and(crate::machine_employee::requires_admin))
            {
                results.push(json!({"index":index,"error":"member_agent_restricted"})); continue;
            }
            let work = db.clone();
            let actor = ctx.user_id.clone();
            let epoch = ctx.credential_epoch;
            let created = tokio::task::spawn_blocking(move || work.create_member_at_epoch(&member, &actor, epoch)).await;
            drop(registry);
            match created {
                Ok(Ok(created)) => {
                    let mut result = json!({"index":index,"user":created.user});
                    if let (Some(issued), Some(base)) = (created.invitation, base) {
                        match self.deliver_user_invitation(&db, issued, ctx, &base, locale).await {
                            Ok(invite) => { result["invite"] = invite; }
                            // A revoked actor receives no bearer link. The account
                            // has committed; preserve that fact to prevent a retry.
                            Err(_) => { result["invite"] = json!({"sent":false,"reason":"invite_unavailable"}); }
                        }
                    }
                    results.push(result);
                }
                Ok(Err(code)) => results.push(json!({"index":index,"error":code})),
                Err(_) => results.push(json!({"index":index,"error":"member_storage_unavailable"})),
            }
        }
        WsFrame::ok_response("", json!({"results":results}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, MethodHandler, Arc<UserDb>, UserContext) {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let admin = db.create_user("admin@test.invalid", "Admin", "admin-password", UserRole::Admin).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
        handler.set_test_edition(Some(duduclaw_core::EditionProfile::Enterprise)).await;
        let ctx = db.live_user_context(&admin.id).unwrap().unwrap();
        let created = handler.handle_agents_create_as(json!({"name":"helper","role":"specialist"}), Some(&ctx)).await;
        assert!(matches!(created, WsFrame::Response { ok: true, .. }));
        handler.registry.write().await.scan().await.unwrap();
        (home, handler, db, ctx)
    }
    fn member(email: &str, invite: bool) -> Value {
        json!({"email":email,"display_name":"New teammate","role":"employee","department":"support",
            "primary_agent_id":"helper","bindings":[{"agent_name":"helper","access_level":"operator"}],"invite":invite})
    }
    fn payload(frame: WsFrame) -> Value {
        match frame { WsFrame::Response { ok: true, payload: Some(value), .. } => value,
            other => panic!("expected member result: {other:?}") }
    }
    fn refused(frame: WsFrame) -> bool { matches!(frame, WsFrame::Response { ok: false, .. }) }

    #[test]
    fn member_scope_is_explicit_and_admin_only() {
        assert_eq!(minimum_role("users.create_members"), Some(UserRole::Admin));
        assert_eq!(minimum_role("users.create_members_unknown"), Some(UserRole::Admin));
    }
    #[tokio::test]
    async fn member_batch_is_atomic_per_row_and_reports_delivery_separately() {
        let (_home, handler, db, admin) = fixture().await;
        let result = payload(handler.handle("users.create_members", json!({"public_url":"https://trusted.test",
            "members":[member("first@test.invalid",true), member("invalid",true), member("third@test.invalid",false)]}), &admin).await);
        assert_eq!(result["results"].as_array().unwrap().len(), 3);
        let first = result["results"][0]["user"]["id"].as_str().unwrap();
        assert_eq!(db.primary_agent(first).unwrap().as_deref(), Some("helper"));
        assert_eq!(db.get_user_agents(first).unwrap().len(), 1);
        assert_eq!(result["results"][0]["invite"]["sent"], false);
        assert!(result["results"][0]["invite"]["link"].is_string());
        assert_eq!(result["results"][1]["error"], "member_invalid_email");
        assert!(result["results"][2]["user"]["id"].is_string());
        assert!(result["results"][2].get("invite").is_none());
        assert_eq!(db.list_users().unwrap().len(), 3);
    }
    #[tokio::test]
    async fn member_batch_checks_admin_live_epoch_and_maximum_size() {
        let (home, handler, db, admin) = fixture().await;
        let params = json!({"members":[member("new@test.invalid",false)]});
        let employee = db.create_user("employee@test.invalid", "Employee", "employee-password", UserRole::Employee).unwrap();
        let mut forged = admin.clone(); forged.user_id = employee.id;
        assert!(refused(handler.handle("users.create_members", params.clone(), &forged).await));
        assert!(refused(handler.handle("users.create_members", json!({"members":vec![member("new@test.invalid",false);11]}), &admin).await));
        assert!(refused(handler.handle("users.create_members", json!({"members":[]}), &admin).await));
        rusqlite::Connection::open(home.path().join("users.db")).unwrap()
            .execute("UPDATE users SET credential_epoch=1 WHERE id=?1", [&admin.user_id]).unwrap();
        assert!(refused(handler.handle("users.create_members", params, &admin).await));
        assert_eq!(db.list_users().unwrap().len(), 2);
    }
    #[tokio::test]
    async fn member_rows_refuse_missing_agent_and_bad_url_before_creating_accounts() {
        let (_home, handler, db, admin) = fixture().await;
        let mut missing = member("missing@test.invalid",false);
        missing["primary_agent_id"] = json!("missing");
        missing["bindings"][0]["agent_name"] = json!("missing");
        let result = payload(handler.handle("users.create_members", json!({"public_url":"http://example.com",
            "members":[missing,member("invite@test.invalid",true),member("saved@test.invalid",false)]}), &admin).await);
        assert_eq!(result["results"][0]["error"], "member_agent_unavailable");
        assert_eq!(result["results"][1]["error"], "member_invite_url_required");
        assert!(result["results"][2]["user"]["id"].is_string());
        assert_eq!(db.list_users().unwrap().len(), 2);
    }
}
